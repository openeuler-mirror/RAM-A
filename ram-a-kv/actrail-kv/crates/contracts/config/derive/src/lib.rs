//! Derives the configuration contract and maps Rust documentation to serialized TOML fields.

mod deserialize;
mod fields;
mod overrides;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as Tokens;
use quote::{format_ident, quote};
use serde_derive_internals::{ast, Ctxt, Derive};
use syn::{parse_macro_input, parse_quote, Attribute, DeriveInput, Expr, Lit, Meta};

/// Implements the TOML configuration contract for a Serde-serializable named struct.
///
/// The derive provides deserialization through the same Serde field declarations.
#[proc_macro_derive(Config, attributes(serde))]
pub fn derive_config(input: TokenStream) -> TokenStream {
    expand(&parse_macro_input!(input as DeriveInput))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand(input: &DeriveInput) -> syn::Result<Tokens> {
    let context = Ctxt::new();
    let container = ast::Container::from_ast(
        &context,
        input,
        Derive::Deserialize,
        &format_ident!("__private"),
    );
    context.check()?;
    let Some(ast::Container {
        data: ast::Data::Struct(ast::Style::Struct, fields),
        attrs,
        ident,
        ..
    }) = container
    else {
        return Err(syn::Error::new_spanned(
            input,
            "Config requires a struct with named fields",
        ));
    };

    let contract: syn::Path = parse_quote!(::config_contract);
    let serde = attrs
        .custom_serde_path()
        .cloned()
        .unwrap_or_else(|| parse_quote!(::serde));
    let deserialize = deserialize::expand(input, &serde, &attrs, &fields)?;
    let field_metadata = fields::expand(&fields, &contract);
    let overrides = overrides::expand(input, &fields, &serde);

    let mut generics = input.generics.clone();
    generics
        .make_where_clause()
        .predicates
        .push(parse_quote!(Self: #serde::Serialize + #serde::de::DeserializeOwned));
    let (impl_generics, type_generics, where_clause) = generics.split_for_impl();
    let comments = fields
        .iter()
        .filter(|field| !field.attrs.skip_serializing())
        .map(|field| {
            let member = &field.member;
            let key = field.attrs.name().serialize_name();
            let key = if field.attrs.flatten() {
                quote!(::core::option::Option::None)
            } else {
                quote!(::core::option::Option::Some(#key))
            };
            let docs = field_comments(&field.original.attrs);
            quote! {
                #contract::derive_support::Field::new(&self.#member)
                    .add_comments(table, #key, #docs);
            }
        });

    Ok(quote! {
        #deserialize
        #overrides

        #[automatically_derived]
        impl #impl_generics #contract::Config for #ident #type_generics #where_clause {
            #field_metadata

            fn parse_toml_table(
                table: &#contract::Table,
            ) -> ::core::result::Result<(Self, #contract::ConfigState), #contract::ConfigError> {
                #contract::derive_support::parse(table)
            }

            fn to_toml_table(&self) -> #contract::Table {
                <Self as #contract::Config>::try_to_toml_table(self)
                    .expect("configuration value must be representable in TOML")
            }

            fn try_to_toml_table(
                &self,
            ) -> ::core::result::Result<#contract::Table, #contract::ConfigError> {
                #contract::derive_support::serialize(self)
            }

            fn add_toml_comments(&self, table: &mut #contract::Table) {
                #[allow(unused_imports)]
                use #contract::derive_support::FieldComments as _;
                #(#comments)*
            }
        }
    })
}

fn field_comments(attributes: &[Attribute]) -> String {
    let docs = attributes
        .iter()
        .filter_map(|attribute| match &attribute.meta {
            Meta::NameValue(meta) if meta.path.is_ident("doc") => match &meta.value {
                Expr::Lit(literal) => match &literal.lit {
                    Lit::Str(doc) => Some(doc.value()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        });
    let mut comments = String::new();
    for doc in docs {
        for line in doc.split('\n') {
            let line = line.strip_prefix(' ').unwrap_or(line).trim_end();
            comments.push('#');
            if !line.is_empty() {
                comments.push(' ');
                comments.push_str(line);
            }
            comments.push('\n');
        }
    }
    comments
}
