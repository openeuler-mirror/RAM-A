//! Derives accepted field names and nested traversal from Serde's existing metadata.
use proc_macro2::TokenStream;
use quote::quote;
use serde_derive_internals::ast;
use syn::Path;

pub(crate) fn expand(fields: &[ast::Field<'_>], contract: &Path) -> TokenStream {
    let mut names = Vec::new();
    let mut nested = Vec::new();
    let mut flattened = Vec::new();
    let mut prepare = Vec::new();
    for field in fields {
        let ty = field.ty;
        if field.attrs.flatten() {
            prepare.push(quote! {
                #contract::derive_support::FieldSchema::<#ty>::new().prepare_table(input, defaults);
            });
            names.push(
                quote!(#contract::derive_support::FieldSchema::<#ty>::new().recognizes_field(key)),
            );
            flattened.push(quote! {
                if #contract::derive_support::FieldSchema::<#ty>::new().recognizes_field(key) {
                    #contract::derive_support::FieldSchema::<#ty>::new()
                        .collect_field(key, item, path, unknown);
                    return;
                }
            });
        } else {
            let mut aliases = field.attrs.aliases().clone();
            aliases.insert(field.attrs.name().deserialize_name().clone());
            let aliases: Vec<_> = aliases.into_iter().collect();
            names.push(quote!([#(#aliases),*].contains(&key)));
            let serialized = field.attrs.name().serialize_name();
            prepare.push(quote! {
                if let Some(name) = [#(#aliases),*].into_iter().find(|name| input.contains_key(name)) {
                    if name != #serialized {
                        if let Some(value) = defaults.remove(#serialized) {
                            defaults.insert(name, value);
                        }
                    }
                    if let (Some(input), Some(default)) = (input.get(name), defaults.get_mut(name)) {
                        #contract::derive_support::FieldSchema::<#ty>::new().prepare_nested(input, default);
                    }
                }
            });
            if !field.attrs.skip_deserializing() {
                nested.push(quote! {
                    if [#(#aliases),*].contains(&key) {
                        #contract::derive_support::FieldSchema::<#ty>::new()
                            .collect_nested(item, path, unknown);
                        return;
                    }
                });
            }
        }
    }
    quote! {
        fn prepare_defaults(input: &dyn #contract::toml_edit::TableLike, defaults: &mut dyn #contract::toml_edit::TableLike) {
            #[allow(unused_imports)]
            use #contract::derive_support::FieldKeys as _;
            #(#prepare)*
        }
        fn recognizes_field(key: &str) -> bool {
            #[allow(unused_imports)]
            use #contract::derive_support::FieldKeys as _;
            false #(|| #names)*
        }
        fn collect_unknown_in_field(
            key: &str, item: &#contract::toml_edit::Item,
            path: &str, unknown: &mut ::std::vec::Vec<::std::string::String>,
        ) {
            #[allow(unused_imports)]
            use #contract::derive_support::FieldKeys as _;
            #(#nested)*
            #(#flattened)*
        }
    }
}
