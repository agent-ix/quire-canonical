// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! `#[derive(FixedShape)]` for `quire-canonical`. Use it through
//! `quire_canonical::FixedShape`.
//!
//! The derived `DEPTH` is computed from the `DEPTH` of every field's type,
//! following serde's default (externally tagged) representation:
//!
//! * a unit struct or unit variant is `0`;
//! * a newtype struct is its field's `DEPTH`;
//! * any other struct is `nest` over its fields (one array or object);
//! * a newtype, tuple or struct variant is one more object (the
//!   `{"variant":...}` wrapper) around its payload;
//! * an enum is the deepest of its variants.
//!
//! Every field's type is named, so a type that contains itself refers to its
//! own `DEPTH`, and rustc refuses the cycle (E0391). The value itself is
//! never a limit. Every field, skipped or not, must implement `FixedShape`.
//!
//! The derive reads the fields, not the serde attributes, so the guarantee
//! holds only while serde serializes those fields:
//!
//! * `flatten`, `tag`, `content`, `untagged`, `transparent` and `skip` only
//!   reshape or drop field values, so the derived `DEPTH` is at worst an
//!   overestimate, and a recursive field still fails to compile.
//! * `into`, `serialize_with`, `with` and `remote` route serialization
//!   through arbitrary code that the derive cannot see. For example,
//!   `#[serde(into = "serde_json::Value")]` with a `From` impl that builds a
//!   deep value recurses natively through serde and can overflow the stack.
//!   These carry the same hazard as a hand-written `Serialize` or a literal
//!   `DEPTH`: use them only for code that emits a value of fixed depth.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as Tokens;
use quote::quote;
use syn::{parse_macro_input, parse_quote, Data, DeriveInput, Fields};

/// Derive `quire_canonical::FixedShape`, computing `DEPTH` from every
/// field's `DEPTH`. Each type parameter gets a `FixedShape` bound.
#[proc_macro_derive(FixedShape)]
pub fn derive_fixed_shape(input: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(input as DeriveInput);
    let depth = match &input.data {
        Data::Struct(data) => struct_depth(&data.fields),
        Data::Enum(data) => {
            let variants = data.variants.iter().map(|variant| match &variant.fields {
                Fields::Unit => quote!(0),
                fields => {
                    let payload = struct_depth(fields);
                    quote!(::quire_canonical::nest(&[#payload]))
                }
            });
            quote!(::quire_canonical::deepest(&[#(#variants),*]))
        }
        Data::Union(data) => {
            return syn::Error::new(
                data.union_token.span,
                "FixedShape cannot be derived for a union",
            )
            .to_compile_error()
            .into();
        }
    };
    for parameter in input.generics.type_params_mut() {
        parameter
            .bounds
            .push(parse_quote!(::quire_canonical::FixedShape));
    }
    let name = &input.ident;
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
    quote! {
        impl #impl_generics ::quire_canonical::FixedShape for #name #type_generics #where_clause {
            const DEPTH: usize = #depth;
        }
    }
    .into()
}

/// The `DEPTH` of a struct, or of a variant's payload, with these fields.
fn struct_depth(fields: &Fields) -> Tokens {
    let types: Vec<_> = fields.iter().map(|field| &field.ty).collect();
    match (fields, types.as_slice()) {
        (Fields::Unit, _) => quote!(0),
        (Fields::Unnamed(_), [single]) => {
            quote!(<#single as ::quire_canonical::FixedShape>::DEPTH)
        }
        _ => quote!(::quire_canonical::nest(&[
            #(<#types as ::quire_canonical::FixedShape>::DEPTH),*
        ])),
    }
}
