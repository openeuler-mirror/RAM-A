//! Uses Serde's remote derive to read the business type with its original field attributes.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use serde_derive_internals::{ast, attr};
use syn::punctuated::Punctuated;
use syn::{parse_quote, DeriveInput, GenericParam, Meta, Path, Token, Visibility};

pub(crate) fn expand(
    input: &DeriveInput,
    serde: &Path,
    attrs: &attr::Container,
    fields: &[ast::Field<'_>],
) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let remote_name = name.to_string();
    let helper_name = format_ident!("__ConfigRead");
    let (_, type_generics, _) = input.generics.split_for_impl();
    let mut helper = input.clone();
    helper.ident = helper_name.clone();
    helper.vis = Visibility::Inherited;
    helper
        .attrs
        .retain(|attribute| attribute.path().is_ident("serde"));
    helper
        .attrs
        .push(parse_quote!(#[serde(remote = #remote_name)]));

    let has_container_default = matches!(attrs.default(), attr::Default::Default);
    if has_container_default {
        redirect_container_default(&mut helper)?;
        helper
            .generics
            .make_where_clause()
            .predicates
            .push(parse_quote!(#name #type_generics: ::core::default::Default));
    }

    let mut generics = input.generics.clone();
    generics.params.insert(0, parse_quote!('de));
    let predicates = &mut generics.make_where_clause().predicates;
    for parameter in &input.generics.params {
        if let GenericParam::Lifetime(parameter) = parameter {
            let lifetime = &parameter.lifetime;
            predicates.push(parse_quote!('de: #lifetime));
        }
    }
    for field in fields {
        if let Some(bounds) = field.attrs.de_bound() {
            predicates.extend(bounds.iter().cloned());
        }
    }
    if has_container_default {
        predicates.push(parse_quote!(Self: ::core::default::Default));
    }
    if let Some(bounds) = attrs.de_bound() {
        predicates.extend(bounds.iter().cloned());
    } else {
        for parameter in input.generics.type_params() {
            let parameter = &parameter.ident;
            predicates.push(parse_quote!(#parameter: #serde::Deserialize<'de>));
        }
        for field in fields {
            if matches!(field.attrs.default(), attr::Default::Default) {
                let ty = field.ty;
                predicates.push(parse_quote!(#ty: ::core::default::Default));
            }
        }
    }
    let (impl_generics, _, where_clause) = generics.split_for_impl();

    Ok(quote! {
        const _: () = {
            #[derive(#serde::Deserialize)]
            #helper

            #[automatically_derived]
            impl #impl_generics #serde::Deserialize<'de> for #name #type_generics #where_clause {
                fn deserialize<__ConfigDeserializer>(
                    deserializer: __ConfigDeserializer,
                ) -> ::core::result::Result<Self, __ConfigDeserializer::Error>
                where
                    __ConfigDeserializer: #serde::Deserializer<'de>,
                {
                    <#helper_name #type_generics>::deserialize(deserializer)
                }
            }
        };
    })
}

fn redirect_container_default(helper: &mut DeriveInput) -> syn::Result<()> {
    // A remote derive puts the bare default bound on the helper. Use the
    // business type's Default through the path form of the Serde attribute.
    for attribute in &mut helper.attrs {
        let mut attrs =
            attribute.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
        for item in &mut attrs {
            if matches!(item, Meta::Path(path) if path.is_ident("default")) {
                *item = parse_quote!(default = "::core::default::Default::default");
            }
        }
        *attribute = parse_quote!(#[serde(#attrs)]);
    }
    Ok(())
}
