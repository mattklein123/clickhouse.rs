#[cfg(test)]
#[path = "native_row_test.rs"]
mod tests;

use crate::attributes::Attributes;
use proc_macro2::TokenStream;
use quote::quote;
use serde_derive_internals::{
    Ctxt,
    attr::{Container, Default as SerdeDefault, Field},
};
use syn::{Data, DeriveInput, Error, Fields, LitStr, Path, Result, parse_quote};

#[derive(Default)]
struct Options {
    codec: Option<Path>,
    map: bool,
    aggregate: Option<LitStr>,
    argument: Option<LitStr>,
}

impl Options {
    fn parse(field: &syn::Field) -> Result<Self> {
        let mut options = Self::default();
        for attribute in &field.attrs {
            if !attribute.path().is_ident("native") {
                continue;
            }
            attribute.parse_nested_meta(|meta| {
                if meta.path.is_ident("with") && options.codec.is_none() {
                    options.codec = Some(meta.value()?.parse::<LitStr>()?.parse()?);
                } else if meta.path.is_ident("map") && !options.map {
                    options.map = true;
                } else if meta.path.is_ident("aggregate") && options.aggregate.is_none() {
                    options.aggregate = Some(meta.value()?.parse()?);
                } else if meta.path.is_ident("argument") && options.argument.is_none() {
                    options.argument = Some(meta.value()?.parse()?);
                } else {
                    return Err(meta.error("unknown or duplicate Native column option"));
                }
                Ok(())
            })?;
        }
        let modes = usize::from(options.codec.is_some())
            + usize::from(options.map)
            + usize::from(options.aggregate.is_some());
        if modes > 1 || options.aggregate.is_some() != options.argument.is_some() {
            return Err(Error::new_spanned(
                field,
                "use one Native codec; aggregate requires an argument type",
            ));
        }
        Ok(options)
    }
}

pub(super) fn derive(input: DeriveInput) -> Result<TokenStream> {
    let Attributes { crate_path } = input.attrs[..].try_into()?;
    let Data::Struct(data) = &input.data else {
        return Err(Error::new_spanned(
            &input,
            "NativeRow requires a named-field struct",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(Error::new_spanned(
            &input,
            "NativeRow requires a named-field struct",
        ));
    };
    let context = Ctxt::new();
    let container = Container::from_ast(&context, &input);
    let mut selected = Vec::new();
    for (index, field) in fields.named.iter().enumerate() {
        let serde = Field::from_ast(&context, index, field, None, &SerdeDefault::None);
        selected.push((
            field,
            serde.skip_serializing(),
            serde.skip_deserializing(),
            serde.flatten(),
            serde.skip_serializing_if().is_some(),
            serde.serialize_with().is_some(),
        ));
    }
    context.check()?;
    if container.type_into().is_some() {
        return Err(Error::new_spanned(
            &input,
            "NativeRow does not support container-level Serde serialization conversions",
        ));
    }
    let mut generics = input.generics.clone();
    let mut columns = Vec::new();
    let mut estimates = Vec::new();
    for (field, skip_write, skip_read, flatten, conditional, custom) in selected {
        if skip_write && skip_read {
            continue;
        }
        if skip_write || skip_read || flatten || conditional {
            return Err(Error::new_spanned(
                field,
                "NativeRow does not support asymmetric skips, conditional fields, or flatten",
            ));
        }
        let options = Options::parse(field)?;
        let name = field.ident.as_ref().expect("named field");
        let ty = &field.ty;
        let index = columns.len();
        let column_name = quote!(<Self as #crate_path::Row>::COLUMN_NAMES[#index]);
        if let (Some(function), Some(argument)) = (options.aggregate, options.argument) {
            columns.push(quote! {
                #crate_path::native::row::append_aggregate(
                    &mut builder, #column_name, #function, #argument,
                    rows.iter_mut().map(|row| &mut row.#name),
                )?;
            });
            estimates.push(quote!(#crate_path::native::row::aggregate_bytes(&self.#name)));
        } else {
            let codec = if let Some(codec) = options.codec {
                quote!(#codec)
            } else if options.map {
                quote!(#crate_path::native::row::Map)
            } else {
                if custom {
                    return Err(Error::new_spanned(
                        field,
                        "a Serde adapter requires an explicit Native codec",
                    ));
                }
                quote!(#crate_path::native::row::DefaultColumn)
            };
            generics.make_where_clause().predicates.push(parse_quote!(
                #codec: #crate_path::native::row::ColumnCodec<#ty>
            ));
            columns.push(quote! {
                <#codec as #crate_path::native::row::ColumnCodec<#ty>>::append(
                    &mut builder, #column_name,
                    rows.iter_mut().map(|row| &mut row.#name),
                )?;
            });
            estimates.push(quote!(<#codec as #crate_path::native::row::ColumnCodec<#ty>>::estimated_bytes(&self.#name)));
        }
    }
    if columns.is_empty() {
        return Err(Error::new_spanned(
            &input,
            "NativeRow requires at least one column",
        ));
    }
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics #crate_path::native::NativeRow for #name #ty_generics #where_clause {
            fn block(mut rows: ::std::vec::Vec<Self>) -> #crate_path::error::Result<#crate_path::native::Block> {
                let mut builder = #crate_path::native::builder::BlockBuilder::new();
                #(#columns)*
                builder.build().map_err(|error| #crate_path::error::Error::Other(::std::boxed::Box::new(error)))
            }

            fn estimated_bytes(&self) -> usize {
                128usize #(.saturating_add(#estimates))*
            }
        }
    })
}
