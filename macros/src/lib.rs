use crate::attributes::Attributes;
use proc_macro2::{Span, TokenStream};
use quote::quote;
use serde_derive_internals::{
    Ctxt,
    attr::{Container, Default as SerdeDefault, Field},
};
use syn::{Data, DataStruct, DeriveInput, Error, Fields, Lifetime, Result, parse_macro_input};

mod attributes;

#[cfg(test)]
mod tests;

// TODO: support wrappers `Wrapper(Inner)` and `Wrapper<T>(T)`.
// TODO: support the `nested` attribute.
#[proc_macro_derive(Row, attributes(clickhouse))]
pub fn row(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    row_impl(input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn column_names(data: &DataStruct, cx: &Ctxt, container: &Container) -> Result<TokenStream> {
    Ok(match &data.fields {
        Fields::Named(fields) => {
            let rename_rule = container.rename_all_rules().deserialize;
            let column_names_iter = fields
                .named
                .iter()
                .enumerate()
                .map(|(index, field)| Field::from_ast(cx, index, field, None, &SerdeDefault::None))
                .filter(|field| !field.skip_serializing() && !field.skip_deserializing())
                .map(|field| {
                    rename_rule
                        .apply_to_field(field.name().serialize_name())
                        .to_string()
                });

            quote! {
                &[#( #column_names_iter,)*]
            }
        }
        Fields::Unnamed(_) => {
            quote! { &[] }
        }
        Fields::Unit => unreachable!("checked by the caller"),
    })
}

fn field_has_raw_binary(field: &syn::Field) -> Result<bool> {
    for attr in &field.attrs {
        if !attr.path().is_ident("clickhouse") {
            continue;
        }

        let mut is_raw_binary = false;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("raw_binary") {
                is_raw_binary = true;
                Ok(())
            } else {
                Err(meta.error("unexpected `#[clickhouse(...)]` argument"))
            }
        })?;

        if is_raw_binary {
            return Ok(true);
        }
    }

    Ok(false)
}

fn derives_deserialize(input: &DeriveInput) -> bool {
    input.attrs.iter().any(|attr| {
        if !attr.path().is_ident("derive") {
            return false;
        }

        let mut has_deserialize = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("Deserialize") {
                has_deserialize = true;
            }
            Ok(())
        });

        has_deserialize
    })
}

fn row_impl(input: DeriveInput) -> Result<TokenStream> {
    let cx = Ctxt::new();

    let Attributes { crate_path } = input.attrs[..].try_into()?;

    let container = Container::from_ast(&cx, &input);
    let name = input.ident.clone();

    let result = match &input.data {
        Data::Struct(data) if data.fields.is_empty() => {
            let reason = "`Row` cannot be derived for unit or empty structs";
            Err(Error::new(name.span(), reason))
        }
        Data::Struct(data) => column_names(data, &cx, &container),
        Data::Enum(_) | Data::Union(_) => {
            let reason = "`Row` can only be derived for structs";
            Err(Error::new(name.span(), reason))
        }
    };

    cx.check()?;
    let column_names = result?;

    let value = match input.generics.lifetimes().count() {
        // An owned row: `struct Row { .. }`
        0 => quote! { Self },
        // A borrowed row: `struct Row<'a> { .. }`
        1 => {
            // Replace the lifetime with `__v` to set `Value<'__v> = ..`.
            let mut cloned = input.generics.clone();
            let param = cloned.lifetimes_mut().next().unwrap();
            param.lifetime = Lifetime::new("'__v", Span::call_site());
            let ty_generics = cloned.split_for_impl().1;
            quote! { #name #ty_generics }
        }
        // A borrowed row with multiple lifetimes: `struct Row<'a, 'b> { .. }`
        _ => {
            let lt = input.generics.lifetimes().nth(1).unwrap();
            let reason = "`Row` cannot be derived for structs with multiple lifetimes";
            return Err(Error::new(lt.lifetime.span(), reason));
        }
    };

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let raw_fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => fields
                .named
                .iter()
                .map(|field| field_has_raw_binary(field))
                .collect::<Result<Vec<_>>>()?,
            Fields::Unnamed(fields) => fields
                .unnamed
                .iter()
                .map(|field| field_has_raw_binary(field))
                .collect::<Result<Vec<_>>>()?,
            Fields::Unit => vec![],
        },
        _ => vec![],
    };

    let has_raw_binary = raw_fields.iter().any(|value| *value);

    if has_raw_binary && derives_deserialize(&input) {
        return Err(Error::new(
            name.span(),
            "`Row` with #[clickhouse(raw_binary)] cannot derive `Deserialize`",
        ));
    }

    let rowbinary_decode_impl = if has_raw_binary {
        let (fields, field_is_raw): (Vec<_>, Vec<_>) = match &input.data {
            Data::Struct(data) => match &data.fields {
                Fields::Named(fields) => (
                    fields
                        .named
                        .iter()
                        .map(|field| field.ident.clone().expect("named field"))
                        .collect(),
                    raw_fields.clone(),
                ),
                Fields::Unnamed(_) => {
                    return Err(Error::new(
                        name.span(),
                        "raw binary fields are not supported in tuple structs",
                    ));
                }
                Fields::Unit => {
                    return Err(Error::new(
                        name.span(),
                        "raw binary fields are not supported in unit structs",
                    ));
                }
            },
            _ => (Vec::new(), Vec::new()),
        };

        let deserialize_fields: Vec<_> = fields
            .iter()
            .zip(field_is_raw.iter())
            .map(|(field, is_raw)| {
                if *is_raw {
                    quote! {
                        let #field = #crate_path::serde::RawBinaryRead::deserialize_raw_binary(&mut deserializer)?;
                    }
                } else {
                    quote! {
                        let #field = ::serde::Deserialize::deserialize(&mut deserializer)?;
                    }
                }
            })
            .collect();

        let construct = match &input.data {
            Data::Struct(DataStruct {
                fields: Fields::Named(_),
                ..
            }) => {
                quote! { #name { #( #fields, )* } }
            }
            Data::Struct(DataStruct {
                fields: Fields::Unnamed(_),
                ..
            }) => {
                quote! { #name( #( #fields, )* ) }
            }
            _ => quote! { #name },
        };

        quote! {
            #[automatically_derived]
            impl #impl_generics #crate_path::RowBinaryDecode for #name #ty_generics #where_clause {
                fn decode_rowbinary<'data>(
                    input: &mut &'data [u8],
                    metadata: ::std::option::Option<&#crate_path::_priv::RowMetadata>,
                ) -> #crate_path::_priv::Result<<#name #ty_generics as #crate_path::Row>::Value<'data>> {
                    match metadata {
                        Some(metadata) => {
                            let validator = #crate_path::_priv::DataTypeValidator::<#name #ty_generics>::new(metadata);
                            let mut deserializer = #crate_path::_priv::RowBinaryDeserializer::<#name #ty_generics, _>::new(
                                input,
                                validator,
                            );
                            #( #deserialize_fields )*
                            Ok(#construct)
                        }
                        None => {
                            let mut deserializer = #crate_path::_priv::RowBinaryDeserializer::<#name #ty_generics, _>::new(
                                input,
                                (),
                            );
                            #( #deserialize_fields )*
                            Ok(#construct)
                        }
                    }
                }
            }
        }
    } else {
        quote! {}
    };

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics #crate_path::Row for #name #ty_generics #where_clause {
            const NAME: &'static str = stringify!(#name);
            const COLUMN_NAMES: &'static [&'static str] = #column_names;
            const COLUMN_COUNT: usize = <Self as #crate_path::Row>::COLUMN_NAMES.len();
            const KIND: #crate_path::_priv::RowKind = #crate_path::_priv::RowKind::Struct;

            type Value<'__v> = #value;
        }

        #rowbinary_decode_impl
    })
}
