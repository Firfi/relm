/*
 * Copyright (c) 2017 Boucher, Antoni <bouanto@zoho.com>
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy of
 * this software and associated documentation files (the "Software"), to deal in
 * the Software without restriction, including without limitation the rights to
 * use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
 * the Software, and to permit persons to whom the Software is furnished to do so,
 * subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS
 * FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
 * COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
 * IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
 * CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
 */

/*
 * TODO: automatically add the model() method with a () return type when it is not found?
 * FIXME: Doing model.text.push_str() will not cause a set_text() to be added.
 * TODO: allow giving a name to a widget so that it can be used in the update() method.
 * TODO: does an attribute #[msg] would simplify the implementation instead of #[derive(Msg)]?
 * TODO: allow pattern matching by creating a function update(&mut self, Quit: Msg, model: &mut Model) so that we can separate
 * the update function in multiple functions.
 * TODO: provide default parameter for constructor (like Toplevel). Is it still necessary?
 * Perhaps for construct-only properties (if they don't have a default value, does this happen?).
 * TODO: think about conditions and loops (widget-list).
 */

#![feature(proc_macro)]

#[macro_use]
extern crate lazy_static;
extern crate proc_macro;
#[macro_use]
extern crate quote;
extern crate syn;

mod adder;
mod gen;
mod parser;
mod walker;

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use proc_macro::TokenStream;
use quote::{Tokens, ToTokens};
use syn::{Delimited, FunctionRetTy, Ident, ImplItem, Mac, MethodSig, TokenTree, parse_expr, parse_item};
use syn::FnArg::Captured;
use syn::fold::Folder;
use syn::ItemKind::Impl;
use syn::ImplItemKind::{Const, Macro, Method, Type};
use syn::Ty;
use syn::visit::Visitor;

use adder::{Adder, Property};
use gen::gen;
use parser::{Widget, parse};
use parser::Widget::Gtk;
use walker::ModelVariableVisitor;

type PropertyModelMap = HashMap<Ident, HashSet<Property>>;

struct Component {
    model_type: Ty,
    msg_type: Ty,
    view_type: Ident,
}

lazy_static! {
    static ref COMPONENTS: Mutex<HashMap<Ident, Component>> = Mutex::new(HashMap::new());
}

#[derive(Debug)]
struct State {
    container_method: Option<ImplItem>,
    container_type: Option<ImplItem>,
    model_type: Option<ImplItem>,
    msg_type: Option<Ty>,
    properties_model_map: Option<PropertyModelMap>,
    root_widget: Option<Ident>,
    root_widget_type: Option<Ident>,
    update_method: Option<ImplItem>,
    view_macro: Option<Mac>,
    widget_model_type: Option<Ty>,
    widgets: HashMap<Ident, Ident>, // Map widget ident to widget type.
}

impl State {
    fn new() -> Self {
        State {
            container_method: None,
            container_type: None,
            model_type: None,
            msg_type: None,
            properties_model_map: None,
            root_widget: None,
            root_widget_type: None,
            update_method: None,
            view_macro: None,
            widget_model_type: None,
            widgets: HashMap::new(),
        }
    }
}

#[proc_macro_attribute]
pub fn widget(_attributes: TokenStream, input: TokenStream) -> TokenStream {
    let source = input.to_string();
    let mut ast = parse_item(&source).unwrap();
    if let Impl(unsafety, polarity, generics, path, typ, items) = ast.node {
        let name = get_name(&typ);
        let mut new_items = vec![];
        let mut state = State::new();
        for item in items {
            let i = item.clone();
            match item.node {
                Const(_, _) => panic!("Unexpected const item"),
                Macro(mac) => state.view_macro = Some(mac),
                Method(sig, _) => {
                    match item.ident.to_string().as_ref() {
                        "container" => state.container_method = Some(i),
                        "model" => {
                            state.widget_model_type = Some(get_return_type(sig));
                            new_items.push(i);
                        },
                        "subscriptions" => new_items.push(i),
                        "update" => {
                            state.msg_type = Some(get_second_param_type(sig));
                            state.update_method = Some(i)
                        },
                        "update_command" => new_items.push(i), // TODO: automatically create this function from the events present in the view (or by splitting the update() fucntion).
                        method_name => panic!("Unexpected method {}", method_name),
                    }
                },
                Type(_) => {
                    if item.ident == Ident::new("Container") {
                        state.container_type = Some(i);
                    }
                    else if item.ident == Ident::new("Model") {
                        state.model_type = Some(i);
                    }
                    else {
                        panic!("Unexpected type item {:?}", item.ident);
                    }
                },
            }
        }
        let (view, map, relm_widgets) = get_view(&name, &mut state);
        state.properties_model_map = Some(map);
        new_items.push(view);
        state.widgets.insert(state.root_widget.clone().expect("root widget"),
            state.root_widget_type.clone().expect("root widget type"));
        new_items.push(get_model_type(state.model_type, state.widget_model_type));
        new_items.push(get_container_type(state.container_type, state.root_widget_type));
        new_items.push(get_update(state.update_method.expect("update method"),
            state.properties_model_map.expect("properties model map")));
        new_items.push(get_container(state.container_method, state.root_widget));
        let item = Impl(unsafety, polarity, generics, path, typ, new_items);
        ast.node = item;
        let widget_struct = create_struct(&name, state.widgets, relm_widgets);
        let expanded = quote! {
            #widget_struct
            #ast
        };
        expanded.parse().unwrap()
    }
    else {
        panic!("Expected impl");
    }
}

fn add_widgets(widget: &Widget, widgets: &mut HashMap<Ident, Ident>, map: &PropertyModelMap) {
    // Only add widgets that are needed by the update() function.
    let mut to_add = false;
    for values in map.values() {
        for value in values {
            if value.widget_name == widget.name() {
                to_add = true;
            }
        }
    }
    if to_add {
        widgets.insert(widget.name().clone(), widget.typ().clone());
    }
    if let Gtk(ref gtk_widget) = *widget {
        for child in &gtk_widget.children {
            add_widgets(child, widgets, map);
        }
    }
}

fn block_to_impl_item(tokens: Tokens) -> ImplItem {
    let implementation = quote! {
        impl Test {
            #tokens
        }
    };
    let implementation = parse_item(implementation.as_str()).unwrap();
    match implementation.node {
        Impl(_, _, _, _, _, items) => items[0].clone(),
        _ => unreachable!(),
    }
}

fn create_struct(name: &Ident, widgets: HashMap<Ident, Ident>, relm_widgets: HashMap<Ident, Ident>) -> Tokens {
    let idents = widgets.keys();
    let types = widgets.values();
    let relm_idents = relm_widgets.keys();
    let relm_types = relm_widgets.values();
    quote! {
        struct #name {
            #(#idents: #types,)*
            #(#relm_idents: #relm_types,)*
        }
    }
}

fn impl_view(name: &Ident, state: &mut State) -> (ImplItem, PropertyModelMap, HashMap<Ident, Ident>) {
    let tokens = &state.view_macro.as_ref().unwrap().tts;
    if let TokenTree::Delimited(Delimited { ref tts, .. }) = tokens[0] {
        let mut widget = parse(tts);
        if let Gtk(ref mut widget) = widget {
            widget.relm_name = Some(name.clone());
            let mut components = COMPONENTS.lock().unwrap();
            components.insert(name.clone(), Component {
                msg_type: state.msg_type.clone().unwrap(),
                model_type: state.widget_model_type.clone().unwrap(),
                view_type: widget.gtk_type.clone(),
            });
        }
        let mut properties_model_map = HashMap::new();
        get_properties_model_map(&widget, &mut properties_model_map);
        add_widgets(&widget, &mut state.widgets, &properties_model_map);
        let idents = state.widgets.keys().collect();
        let (view, relm_widgets) = gen(name, widget, &mut state.root_widget, &mut state.root_widget_type, idents);
        let event_type = &state.msg_type;
        let item = block_to_impl_item(quote! {
            #[allow(unused_variables)] // Necessary to avoid warnings in case the parameters are unused.
            fn view(relm: RemoteRelm<#event_type>, model: &Self::Model) -> Self {
                #view
            }
        });
        (item, properties_model_map, relm_widgets)
    }
    else {
        panic!("Expected `{{` but found `{:?}` in view! macro", tokens[0]);
    }
}

fn get_container(container_method: Option<ImplItem>, root_widget: Option<Ident>) -> ImplItem {
    container_method.unwrap_or_else(|| {
        let root_widget = root_widget.expect("root widget");
        block_to_impl_item(quote! {
            fn container(&self) -> &Self::Container {
                &self.#root_widget
            }
        })
    })
}

fn get_container_type(container_type: Option<ImplItem>, root_widget_type: Option<Ident>) -> ImplItem {
    container_type.unwrap_or_else(|| {
        let root_widget_type = root_widget_type.expect("root widget type");
        block_to_impl_item(quote! {
            type Container = #root_widget_type;
        })
    })
}

fn get_model_type(model_type: Option<ImplItem>, widget_model_type: Option<Ty>) -> ImplItem {
    model_type.unwrap_or_else(|| {
        let widget_model_type = widget_model_type.expect("widget model type");
        block_to_impl_item(quote! {
            type Model = #widget_model_type;
        })
    })
}

fn get_name(typ: &Ty) -> Ident {
    if let Ty::Path(_, ref path) = *typ {
        let mut tokens = Tokens::new();
        path.to_tokens(&mut tokens);
        Ident::new(tokens.to_string())
    }
    else {
        panic!("Expected Path")
    }
}

/*
 * TODO: Create a control flow graph for each variable of the model.
 * Add the set_property() calls in every leaf of every graphs.
 */
fn get_update(mut func: ImplItem, map: PropertyModelMap) -> ImplItem {
    if let Method(_, ref mut block) = func.node {
        let mut adder = Adder::new(&map);
        *block = adder.fold_block(block.clone());
    }
    // TODO: consider gtk::main_quit() as return.
    func
}

fn get_view(name: &Ident, state: &mut State) -> (ImplItem, PropertyModelMap, HashMap<Ident, Ident>) {
    {
        let ref segments = state.view_macro.as_ref().unwrap().path.segments;
        if segments.len() != 1 || segments[0].ident.to_string() != "view" {
            panic!("Unexpected macro item")
        }
    }
    impl_view(name, state)
}

/*
 * The map maps model variable name to a vector of tuples (widget name, property name).
 */
fn get_properties_model_map(widget: &Widget, map: &mut PropertyModelMap) {
    if let Gtk(ref gtk_widget) = *widget {
        for (name, value) in &gtk_widget.properties {
            let string = value.parse::<String>().unwrap();
            let expr = parse_expr(&string).unwrap();
            let mut visitor = ModelVariableVisitor::new();
            visitor.visit_expr(&expr);
            let model_variables = visitor.idents;
            for var in model_variables {
                let set = map.entry(var).or_insert_with(HashSet::new);
                set.insert(Property {
                    expr: string.clone(),
                    name: name.clone(),
                    widget_name: gtk_widget.name.clone(),
                });
            }
        }
        for child in &gtk_widget.children {
            get_properties_model_map(child, map);
        }
    }
}

fn get_return_type(sig: MethodSig) -> Ty {
    if let FunctionRetTy::Ty(ty) = sig.decl.output {
        ty
    }
    else {
        panic!("Unexpected default, expecting Ty");
    }
}

fn get_second_param_type(sig: MethodSig) -> Ty {
    if let Captured(_, ref path) = sig.decl.inputs[1] {
        path.clone()
    }
    else {
        panic!("Unexpected `{:?}`, expecting Captured Ty", sig.decl.inputs[1]);
    }
}