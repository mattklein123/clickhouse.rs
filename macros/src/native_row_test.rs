use super::derive;
use syn::{DeriveInput, ItemImpl, parse_quote};

#[test]
fn generates_owned_columns_and_custom_codecs() {
    let output = derive(parse_quote! {
        struct Sample {
            value: u64,
            #[native(map)]
            fields: Vec<(String, String)>,
            #[native(with = "Timestamp")]
            time: Time,
            #[native(aggregate = "uniqCombined(14)", argument = "String")]
            state: RawBinary,
        }
    })
    .unwrap();
    syn::parse2::<ItemImpl>(output.clone()).unwrap();
    let output = output.to_string();
    assert!(output.contains("rows . iter_mut ()"));
    assert!(output.contains("append_aggregate"));
    assert!(output.contains("uniqCombined(14)"));
    assert!(output.contains("COLUMN_NAMES [3usize]"));
}

#[test]
fn rejects_invalid_shapes_and_options() {
    let inputs: [DeriveInput; 12] = [
        parse_quote!(
            enum Sample {
                Value,
            }
        ),
        parse_quote!(
            struct Sample(u64);
        ),
        parse_quote!(
            struct Sample {}
        ),
        parse_quote!(
            struct Sample {
                #[native(unknown)]
                value: u64,
            }
        ),
        parse_quote!(
            struct Sample {
                #[native(map, map)]
                value: u64,
            }
        ),
        parse_quote!(
            struct Sample {
                #[native(map, with = "Other")]
                value: u64,
            }
        ),
        parse_quote!(
            struct Sample {
                #[native(aggregate = "sum")]
                value: u64,
            }
        ),
        parse_quote!(
            struct Sample {
                #[native(argument = "UInt64")]
                value: u64,
            }
        ),
        parse_quote!(
            struct Sample {
                #[serde(flatten)]
                value: u64,
            }
        ),
        parse_quote!(
            struct Sample {
                #[serde(skip_serializing)]
                value: u64,
            }
        ),
        parse_quote!(
            struct Sample {
                #[serde(with = "adapter")]
                value: u64,
            }
        ),
        parse_quote!(
            struct Sample {
                #[serde(skip)]
                value: u64,
            }
        ),
    ];
    for input in inputs {
        assert!(derive(input).is_err());
    }
}

#[test]
fn rejects_container_serialization_conversions() {
    let input = parse_quote! {
        #[serde(into = "Serialized")]
        struct Sample {
            value: u64,
        }
    };
    let error = derive(input).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("container-level Serde serialization conversions")
    );
}

#[test]
fn supports_generics_crate_aliases_and_skipped_fields() {
    let output = derive(parse_quote! {
        #[clickhouse(crate = "renamed")]
        struct Sample<Value> {
            #[serde(skip)]
            ignored: String,
            value: Value,
        }
    })
    .unwrap();
    syn::parse2::<ItemImpl>(output.clone()).unwrap();
    let output = output.to_string();
    assert!(output.contains("renamed :: native :: NativeRow"));
    assert!(output.contains("COLUMN_NAMES [0usize]"));
    assert!(!output.contains("row . ignored"));
}
