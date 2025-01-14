use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{
    parenthesized,
    parse::{Parse, ParseStream},
    spanned::Spanned,
    Token,
};

use crate::{
    diagnostic::{DiagnosticConcreteArgs, DiagnosticDef},
    fmt::{self, Display},
    forward::WhichFn,
    utils::{display_pat_members, gen_all_variants_with},
};

pub struct Labels(Vec<Label>);

struct Label {
    label: Option<Display>,
    ty: syn::Type,
    span: syn::Member,
    primary: bool,
}

struct LabelAttr {
    label: Option<Display>,
    primary: bool,
}

impl Parse for LabelAttr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Skip a token.
        // This should receive one of:
        // - label = "..."
        // - label("...")
        let _ = input.step(|cursor| {
            if let Some((_, next)) = cursor.token_tree() {
                Ok(((), next))
            } else {
                Err(cursor.error("unexpected empty attribute"))
            }
        });
        let la = input.lookahead1();
        let (primary, label) = if la.peek(syn::token::Paren) {
            // #[label(primary?, "{}", x)]
            let content;
            parenthesized!(content in input);

            let primary = if content.peek(syn::Ident) {
                let ident: syn::Ident = content.parse()?;
                if ident != "primary" {
                    return Err(syn::Error::new(input.span(), "Invalid argument to label() attribute. The argument must be a literal string or the keyword `primary`."));
                }
                let _ = content.parse::<Token![,]>();
                true
            } else {
                false
            };

            if content.peek(syn::LitStr) {
                let fmt = content.parse()?;
                let args = if content.is_empty() {
                    TokenStream::new()
                } else {
                    fmt::parse_token_expr(&content, false)?
                };
                let display = Display {
                    fmt,
                    args,
                    has_bonus_display: false,
                };
                (primary, Some(display))
            } else if !primary {
                return Err(syn::Error::new(input.span(), "Invalid argument to label() attribute. The argument must be a literal string or the keyword `primary`."));
            } else {
                (primary, None)
            }
        } else if la.peek(Token![=]) {
            // #[label = "blabla"]
            input.parse::<Token![=]>()?;
            (
                false,
                Some(Display {
                    fmt: input.parse()?,
                    args: TokenStream::new(),
                    has_bonus_display: false,
                }),
            )
        } else {
            (false, None)
        };
        Ok(LabelAttr { label, primary })
    }
}

impl Labels {
    pub fn from_fields(fields: &syn::Fields) -> syn::Result<Option<Self>> {
        match fields {
            syn::Fields::Named(named) => Self::from_fields_vec(named.named.iter().collect()),
            syn::Fields::Unnamed(unnamed) => {
                Self::from_fields_vec(unnamed.unnamed.iter().collect())
            }
            syn::Fields::Unit => Ok(None),
        }
    }

    fn from_fields_vec(fields: Vec<&syn::Field>) -> syn::Result<Option<Self>> {
        let mut labels = Vec::new();
        for (i, field) in fields.iter().enumerate() {
            for attr in &field.attrs {
                if attr.path().is_ident("label") {
                    let span = if let Some(ident) = field.ident.clone() {
                        syn::Member::Named(ident)
                    } else {
                        syn::Member::Unnamed(syn::Index {
                            index: i as u32,
                            span: field.span(),
                        })
                    };
                    use quote::ToTokens;
                    let LabelAttr { label, primary } =
                        syn::parse2::<LabelAttr>(attr.meta.to_token_stream())?;

                    if primary && labels.iter().any(|l: &Label| l.primary) {
                        return Err(syn::Error::new(
                            field.span(),
                            "Cannot have more than one primary label.",
                        ));
                    }

                    labels.push(Label {
                        label,
                        span,
                        ty: field.ty.clone(),
                        primary,
                    });
                }
            }
        }
        if labels.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Labels(labels)))
        }
    }

    pub(crate) fn gen_struct(&self, fields: &syn::Fields) -> Option<TokenStream> {
        let (display_pat, display_members) = display_pat_members(fields);
        let labels = self.0.iter().map(|highlight| {
            let Label {
                span,
                label,
                ty,
                primary,
            } = highlight;
            let var = quote! { __miette_internal_var };
            let ctor = if *primary {
                quote! { miette::LabeledSpan::new_primary_with_span }
            } else {
                quote! { miette::LabeledSpan::new_with_span }
            };
            if let Some(display) = label {
                let (fmt, args) = display.expand_shorthand_cloned(&display_members);
                quote! {
                    miette::macro_helpers::OptionalWrapper::<#ty>::new().to_option(&self.#span)
                    .map(|#var| #ctor(
                        std::option::Option::Some(format!(#fmt #args)),
                        #var.clone(),
                    ))
                }
            } else {
                quote! {
                    miette::macro_helpers::OptionalWrapper::<#ty>::new().to_option(&self.#span)
                    .map(|#var| #ctor(
                        std::option::Option::None,
                        #var.clone(),
                    ))
                }
            }
        });
        Some(quote! {
            #[allow(unused_variables)]
            fn labels(&self) -> std::option::Option<std::boxed::Box<dyn std::iter::Iterator<Item = miette::LabeledSpan> + '_>> {
                use miette::macro_helpers::ToOption;
                let Self #display_pat = self;
                std::option::Option::Some(Box::new(vec![
                    #(#labels),*
                ].into_iter().filter(Option::is_some).map(Option::unwrap)))
            }
        })
    }

    pub(crate) fn gen_enum(variants: &[DiagnosticDef]) -> Option<TokenStream> {
        gen_all_variants_with(
            variants,
            WhichFn::Labels,
            |ident, fields, DiagnosticConcreteArgs { labels, .. }| {
                let (display_pat, display_members) = display_pat_members(fields);
                labels.as_ref().and_then(|labels| {
                    let variant_labels = labels.0.iter().map(|label| {
                        let Label { span, label, ty, primary } = label;
                        let field = match &span {
                            syn::Member::Named(ident) => ident.clone(),
                            syn::Member::Unnamed(syn::Index { index, .. }) => {
                                format_ident!("_{}", index)
                            }
                        };
                        let var = quote! { __miette_internal_var };
                        let ctor = if *primary {
                            quote! { miette::LabeledSpan::new_primary_with_span }
                        } else {
                            quote! { miette::LabeledSpan::new_with_span }
                        };
                        if let Some(display) = label {
                            let (fmt, args) = display.expand_shorthand_cloned(&display_members);
                            quote! {
                                miette::macro_helpers::OptionalWrapper::<#ty>::new().to_option(#field)
                                .map(|#var| #ctor(
                                    std::option::Option::Some(format!(#fmt #args)),
                                    #var.clone(),
                                ))
                            }
                        } else {
                            quote! {
                                miette::macro_helpers::OptionalWrapper::<#ty>::new().to_option(#field)
                                .map(|#var| #ctor(
                                    std::option::Option::None,
                                    #var.clone(),
                                ))
                            }
                        }
                    });
                    let variant_name = ident.clone();
                    match &fields {
                        syn::Fields::Unit => None,
                        _ => Some(quote! {
                            Self::#variant_name #display_pat => {
                                use miette::macro_helpers::ToOption;
                                std::option::Option::Some(std::boxed::Box::new(vec![
                                    #(#variant_labels),*
                                ].into_iter().filter(Option::is_some).map(Option::unwrap)))
                            }
                        }),
                    }
                })
            },
        )
    }
}
