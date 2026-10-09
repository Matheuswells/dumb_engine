//! `#[derive(Editor)]` and `#[derive(Component)]` for Dumb Engine.
//!
//! ```ignore
//! #[derive(Component, Editor, Default)]
//! struct Player {
//!     #[editor(range = 0.0..=20.0)]
//!     speed: f32,
//!     health: f32,
//!     #[editor(asset = "model")]
//!     weapon: AssetId,
//! }
//! ```
//!
//! Generated code refers to `::dumb_reflect` and `::dumb_ecs`, so a crate using the derives
//! must depend on both (the `dumb_script` prelude documents this).

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{quote, ToTokens};
use syn::{parse_macro_input, Data, DeriveInput, Expr, Fields, LitStr};

#[proc_macro_derive(Editor, attributes(editor))]
pub fn derive_editor(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand_editor(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

#[proc_macro_derive(Component, attributes(component))]
pub fn derive_component(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let (ig, tg, wc) = input.generics.split_for_impl();
    quote! {
        impl #ig ::dumb_ecs::Component for #name #tg #wc {}
    }
    .into()
}

fn type_name_expr(input: &DeriveInput) -> TokenStream2 {
    let name = input.ident.to_string();
    quote! { concat!(module_path!(), "::", #name) }
}

fn expand_editor(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    let (ig, tg, wc) = input.generics.split_for_impl();
    let type_name = type_name_expr(input);

    let common = quote! {
        fn type_name(&self) -> &'static str { #type_name }
        fn as_any(&self) -> &dyn ::std::any::Any { self }
        fn as_any_mut(&mut self) -> &mut dyn ::std::any::Any { self }
    };

    match &input.data {
        Data::Struct(s) => {
            let fields = match &s.fields {
                Fields::Named(f) => f.named.iter().collect::<Vec<_>>(),
                Fields::Unit => Vec::new(),
                Fields::Unnamed(_) => {
                    return Err(syn::Error::new_spanned(
                        name,
                        "#[derive(Editor)] needs named fields",
                    ))
                }
            };

            let mut infos = Vec::new();
            let mut idents = Vec::new();
            for f in fields {
                let attrs = parse_field_attrs(&f.attrs)?;
                if attrs.skip {
                    continue;
                }
                let ident = f.ident.clone().unwrap();
                let fname = ident.to_string();
                let a = attrs.tokens;
                infos.push(quote! { ::dumb_reflect::FieldInfo { name: #fname, attrs: #a } });
                idents.push(ident);
            }
            let count = infos.len();
            let idx: Vec<usize> = (0..count).collect();

            Ok(quote! {
                impl #ig ::dumb_reflect::Struct for #name #tg #wc {
                    fn fields(&self) -> &'static [::dumb_reflect::FieldInfo] {
                        static FIELDS: [::dumb_reflect::FieldInfo; #count] = [#(#infos),*];
                        &FIELDS
                    }
                    fn field(&self, index: usize) -> &dyn ::dumb_reflect::Reflect {
                        match index {
                            #(#idx => &self.#idents,)*
                            _ => panic!("field index out of range"),
                        }
                    }
                    fn field_mut(&mut self, index: usize) -> &mut dyn ::dumb_reflect::Reflect {
                        match index {
                            #(#idx => &mut self.#idents,)*
                            _ => panic!("field index out of range"),
                        }
                    }
                }

                impl #ig ::dumb_reflect::Reflect for #name #tg #wc {
                    #common
                    fn reflect_ref(&self) -> ::dumb_reflect::ReflectRef<'_> {
                        ::dumb_reflect::ReflectRef::Struct(self)
                    }
                    fn reflect_mut(&mut self) -> ::dumb_reflect::ReflectMut<'_> {
                        ::dumb_reflect::ReflectMut::Struct(self)
                    }
                }

                impl #ig ::dumb_reflect::Typed for #name #tg #wc {
                    const TYPE_NAME: &'static str = #type_name;
                }
            })
        }
        Data::Enum(e) => {
            let mut names = Vec::new();
            let mut variants = Vec::new();
            for v in &e.variants {
                if !matches!(v.fields, Fields::Unit) {
                    return Err(syn::Error::new_spanned(
                        v,
                        "#[derive(Editor)] supports only fieldless enums for now",
                    ));
                }
                names.push(v.ident.to_string());
                variants.push(v.ident.clone());
            }
            let count = names.len();
            let idx: Vec<usize> = (0..count).collect();
            Ok(quote! {
                impl #ig ::dumb_reflect::Enum for #name #tg #wc {
                    fn variants(&self) -> &'static [&'static str] {
                        static NAMES: [&str; #count] = [#(#names),*];
                        &NAMES
                    }
                    fn variant_index(&self) -> usize {
                        match self { #(Self::#variants => #idx,)* }
                    }
                    fn set_variant_index(&mut self, index: usize) {
                        *self = match index { #(#idx => Self::#variants,)* _ => return };
                    }
                }

                impl #ig ::dumb_reflect::Reflect for #name #tg #wc {
                    #common
                    fn reflect_ref(&self) -> ::dumb_reflect::ReflectRef<'_> {
                        ::dumb_reflect::ReflectRef::Enum(self)
                    }
                    fn reflect_mut(&mut self) -> ::dumb_reflect::ReflectMut<'_> {
                        ::dumb_reflect::ReflectMut::Enum(self)
                    }
                }

                impl #ig ::dumb_reflect::Typed for #name #tg #wc {
                    const TYPE_NAME: &'static str = #type_name;
                }
            })
        }
        Data::Union(_) => Err(syn::Error::new_spanned(name, "unions cannot derive Editor")),
    }
}

struct ParsedAttrs {
    skip: bool,
    tokens: TokenStream2,
}

fn parse_field_attrs(attrs: &[syn::Attribute]) -> syn::Result<ParsedAttrs> {
    let mut skip = false;
    let mut range = quote!(None);
    let mut speed = quote!(None);
    let mut readonly = false;
    let mut hidden = false;
    let mut color = false;
    let mut tooltip = quote!(None);
    let mut asset = quote!(None);

    for attr in attrs.iter().filter(|a| a.path().is_ident("editor")) {
        attr.parse_nested_meta(|meta| {
            let p = &meta.path;
            if p.is_ident("skip") {
                skip = true;
            } else if p.is_ident("readonly") {
                readonly = true;
            } else if p.is_ident("hidden") {
                hidden = true;
            } else if p.is_ident("color") {
                color = true;
            } else if p.is_ident("range") {
                let expr: Expr = meta.value()?.parse()?;
                match expr {
                    Expr::Range(r) => {
                        let (Some(s), Some(e)) = (r.start, r.end) else {
                            return Err(meta.error("range needs both ends, e.g. 0.0..=1.0"));
                        };
                        range = quote!(Some(((#s) as f64, (#e) as f64)));
                    }
                    _ => return Err(meta.error("expected a range like 0.0..=1.0")),
                }
            } else if p.is_ident("speed") {
                let expr: Expr = meta.value()?.parse()?;
                speed = quote!(Some((#expr) as f64));
            } else if p.is_ident("tooltip") {
                let s: LitStr = meta.value()?.parse()?;
                tooltip = quote!(Some(#s));
            } else if p.is_ident("asset") {
                let s: LitStr = meta.value()?.parse()?;
                asset = quote!(Some(#s));
            } else {
                return Err(meta.error(format!(
                    "unknown editor attribute `{}`",
                    p.to_token_stream()
                )));
            }
            Ok(())
        })?;
    }

    Ok(ParsedAttrs {
        skip,
        tokens: quote! {
            ::dumb_reflect::FieldAttrs {
                range: #range,
                speed: #speed,
                readonly: #readonly,
                hidden: #hidden,
                tooltip: #tooltip,
                asset_kind: #asset,
                color: #color,
            }
        },
    })
}
