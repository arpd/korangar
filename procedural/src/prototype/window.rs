use proc_macro::TokenStream as InterfaceTokenStream;
use quote::quote;
use syn::{Attribute, DataStruct, Generics, Ident};

use super::helper::prototype_element_helper;

pub fn derive_prototype_window_struct(
    data_struct: DataStruct,
    generics: Generics,
    attributes: Vec<Attribute>,
    name: Ident,
) -> InterfaceTokenStream {

    let (initializers, window_title, window_class) = prototype_element_helper(data_struct, attributes, name.to_string());
    let (impl_generics, type_generics, where_clause) = generics.split_for_impl();

    let (window_class_option, window_class_ref_option) = window_class
        .map(|window_class| (quote!(#window_class.to_string().into()), quote!(#window_class.into())))
        .unwrap_or((quote!(None), quote!(None)));

    quote! {
        impl #impl_generics crate::interface::PrototypeWindow for #name #type_generics #where_clause {

            fn window_class(&self) -> Option<&str> {
                #window_class_ref_option
            }

            fn to_window(&self, window_cache: &crate::interface::WindowCache, interface_settings: &crate::interface::InterfaceSettings, avalible_space: crate::interface::Size) -> std::boxed::Box<dyn crate::interface::Window + 'static> {
                let scroll_view = crate::interface::ScrollView::new(vec![#(#initializers),*], constraint!(100%, ?));
                let elements: Vec<crate::interface::ElementCell> = vec![std::rc::Rc::new(std::cell::RefCell::new(scroll_view))];
                let size_constraint = constraint!(200 > 300 < 400, 100 > ? < 80%);
                std::boxed::Box::new(crate::interface::FramedWindow::new(window_cache, interface_settings, avalible_space, #window_title.to_string(), #window_class_option, elements, size_constraint, true))
            }
        }
    }.into()
}
