//! Generates sparse typed overrides from the same field names and nested configuration types.
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use serde_derive_internals::ast;
use syn::{parse_quote, DeriveInput, Path};

pub(crate) fn expand(input: &DeriveInput, fields: &[ast::Field<'_>], serde: &Path) -> TokenStream {
    let ident = &input.ident;
    let name = format_ident!("{}Overrides", ident);
    let visibility = &input.vis;
    let mut generics = input.generics.clone();
    let fields: Vec<_> = fields
        .iter()
        .filter(|field| !field.attrs.skip_serializing())
        .collect();
    for field in &fields {
        let ty = field.ty;
        generics
            .make_where_clause()
            .predicates
            .push(parse_quote!(#ty: ::config_contract::ConfigField));
    }
    let (impl_generics, type_generics, where_clause) = generics.split_for_impl();
    let declarations = fields.iter().map(|field| {
        let member = &field.member;
        let visibility = &field.original.vis;
        let ty = field.ty;
        let key = field.attrs.name().serialize_name();
        let naming = if field.attrs.flatten() { quote!(#[serde(flatten)]) } else { quote!(#[serde(rename = #key)]) };
        let docs = field.original.attrs.iter().filter(|attr| attr.path().is_ident("doc"));
        quote! {
            #(#docs)*
            #naming
            #[serde(skip_serializing_if = "Option::is_none")]
            #visibility #member: ::core::option::Option<<#ty as ::config_contract::ConfigField>::Overrides>,
        }
    });
    let defaults = fields.iter().map(|field| {
        let member = &field.member;
        quote!(#member: None,)
    });
    quote! {
        #[doc = "Explicit field overrides. Unset fields inherit the base configuration."]
        #[derive(#serde::Serialize)]
        #[serde(bound(serialize = ""))]
        #visibility struct #name #impl_generics #where_clause {
            #(#declarations)*
        }
        impl #impl_generics ::core::default::Default for #name #type_generics #where_clause {
            fn default() -> Self { Self { #(#defaults)* } }
        }
        impl #impl_generics ::config_contract::ConfigField for #ident #type_generics #where_clause {
            type Overrides = #name #type_generics;
        }
    }
}
