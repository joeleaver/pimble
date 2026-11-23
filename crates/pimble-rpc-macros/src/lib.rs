//! `#[pimble_api(namespace = "...")]`: one trait, every half of the API.
//!
//! Put on the one trait that lists Pimble's RPC methods
//! (`crates/pimble-rpc/src/methods.rs`), written with jsonrpsee's own
//! `#[method(...)]` and `#[subscription(...)]` attributes. It emits:
//!
//! - the trait itself under jsonrpsee's `#[rpc(client, namespace = ...)]`,
//!   which generates `<Trait>Client` exactly as before (the wire is
//!   jsonrpsee's);
//! - `<Trait>Server`, the trait a server implements: each method's own
//!   signature with `ext: &Extensions` after `&self` when it declares
//!   `with_extensions`, and each subscription's with a
//!   `pending: PendingSubscriptionSink` first, as jsonrpsee's server trait has
//!   them, but over `pimble_rpc::dispatch`'s transport-free sink rather than
//!   jsonrpsee-server's (docs/ONE_CODEBASE_CONTRACT.md seam A);
//! - `METHODS`, every wire name the trait answers to (methods, subscriptions,
//!   their notifications and unsubscribes);
//! - `__call` and `__subscribe`, the dispatch: a wire name and its params
//!   in, the server's answer out, decoding params exactly as jsonrpsee's
//!   server macro does (a JSON array by position, `optional_next` for an
//!   `Option`; a JSON object by name, with snake_case and lowerCamelCase
//!   aliases).
//!
//! All of it from the one list of trait items, so a method cannot be listed
//! in one place and missing from another.

use heck::{ToLowerCamelCase, ToSnakeCase};
use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::parse::{ParseStream, Parser};
use syn::{parse_macro_input, Attribute, FnArg, Ident, ItemTrait, LitStr, Pat, ReturnType, Token, TraitItem, TraitItemFn, Type};

struct Arg {
    ident: Ident,
    ty: Type,
}

enum Kind {
    Method,
    Subscription { notification: String, unsubscribe: String },
}

struct Item {
    kind: Kind,
    /// The wire name, namespace included.
    name: String,
    with_extensions: bool,
    docs: Vec<Attribute>,
    func: TraitItemFn,
    args: Vec<Arg>,
}

#[proc_macro_attribute]
pub fn pimble_api(attr: TokenStream, item: TokenStream) -> TokenStream {
    let namespace = match parse_namespace.parse(attr) {
        Ok(ns) => ns,
        Err(e) => return e.to_compile_error().into(),
    };
    let trait_def = parse_macro_input!(item as ItemTrait);
    match expand(&namespace, trait_def) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn parse_namespace(input: ParseStream) -> syn::Result<String> {
    let key: Ident = input.parse()?;
    if key != "namespace" {
        return Err(syn::Error::new(key.span(), "expected `namespace = \"...\"`"));
    }
    input.parse::<Token![=]>()?;
    let ns: LitStr = input.parse()?;
    Ok(ns.value())
}

/// The keys of a `#[method(...)]` or `#[subscription(...)]` attribute this
/// macro understands. Anything else jsonrpsee offers (aliases, blocking
/// methods, `param_kind`) is refused rather than half-honored, so the
/// client jsonrpsee generates and the dispatch generated here cannot read
/// the same attribute differently.
#[derive(Default)]
struct AttrArgs {
    name: Option<LitStr>,
    notification: Option<LitStr>,
    unsubscribe: Option<LitStr>,
    with_extensions: bool,
}

fn parse_attr_args(input: ParseStream) -> syn::Result<AttrArgs> {
    let mut out = AttrArgs::default();
    while !input.is_empty() {
        let key: Ident = input.parse()?;
        if key == "with_extensions" {
            out.with_extensions = true;
        } else {
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "name" => {
                    out.name = Some(input.parse()?);
                    if input.peek(Token![=>]) {
                        input.parse::<Token![=>]>()?;
                        out.notification = Some(input.parse()?);
                    }
                }
                "unsubscribe" => out.unsubscribe = Some(input.parse()?),
                // The item type matters to the client only; the server sends
                // whatever its sink is given.
                "item" => {
                    input.parse::<Type>()?;
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("`{other}` is not supported by #[pimble_api]: teach pimble-rpc-macros to dispatch it first"),
                    ))
                }
            }
        }
        if !input.is_empty() {
            input.parse::<Token![,]>()?;
        }
    }
    Ok(out)
}

fn expand(namespace: &str, trait_def: ItemTrait) -> syn::Result<TokenStream2> {
    let wire = |name: &str| format!("{namespace}_{name}");
    let mut items = Vec::new();
    for item in &trait_def.items {
        let TraitItem::Fn(func) = item else {
            return Err(syn::Error::new_spanned(item, "#[pimble_api] traits hold methods and subscriptions only"));
        };
        let mut kind_attr = None;
        let mut docs = Vec::new();
        for attr in &func.attrs {
            if attr.path().is_ident("method") || attr.path().is_ident("subscription") {
                if kind_attr.is_some() {
                    return Err(syn::Error::new_spanned(attr, "one #[method] or #[subscription] per item"));
                }
                kind_attr = Some(attr);
            } else if attr.path().is_ident("doc") {
                docs.push(attr.clone());
            }
        }
        let Some(attr) = kind_attr else {
            return Err(syn::Error::new_spanned(&func.sig, "every item needs #[method(...)] or #[subscription(...)]"));
        };
        if func.sig.asyncness.is_none() {
            return Err(syn::Error::new_spanned(&func.sig, "#[pimble_api] items are async"));
        }
        let args_of = attr.parse_args_with(parse_attr_args)?;
        let Some(name) = args_of.name else {
            return Err(syn::Error::new_spanned(attr, "missing `name = \"...\"`"));
        };
        let kind = if attr.path().is_ident("method") {
            if args_of.notification.is_some() || args_of.unsubscribe.is_some() {
                return Err(syn::Error::new_spanned(attr, "a method has no notification or unsubscribe"));
            }
            Kind::Method
        } else {
            let Some(unsubscribe) = args_of.unsubscribe else {
                return Err(syn::Error::new_spanned(attr, "missing `unsubscribe = \"...\"`"));
            };
            Kind::Subscription {
                // jsonrpsee's rule: without `=> "notification"` the
                // notification carries the subscription's own name.
                notification: wire(&args_of.notification.as_ref().unwrap_or(&name).value()),
                unsubscribe: wire(&unsubscribe.value()),
            }
        };
        let mut args = Vec::new();
        for input in func.sig.inputs.iter().skip(1) {
            let FnArg::Typed(pat_ty) = input else {
                return Err(syn::Error::new_spanned(input, "unexpected receiver"));
            };
            let Pat::Ident(pat) = &*pat_ty.pat else {
                return Err(syn::Error::new_spanned(&pat_ty.pat, "parameters are plain names"));
            };
            args.push(Arg { ident: pat.ident.clone(), ty: (*pat_ty.ty).clone() });
        }
        items.push(Item { kind, name: wire(&name.value()), with_extensions: args_of.with_extensions, docs, func: func.clone(), args });
    }

    // Two items answering to one wire name is a compile error, as it is in
    // jsonrpsee's own macro.
    let mut seen = std::collections::HashSet::new();
    for item in &items {
        let mut names = vec![item.name.clone()];
        if let Kind::Subscription { notification, unsubscribe } = &item.kind {
            names.push(unsubscribe.clone());
            if notification != &item.name {
                names.push(notification.clone());
            }
        }
        for name in names {
            if !seen.insert(name.clone()) {
                return Err(syn::Error::new_spanned(&item.func.sig, format!("{name:?} is already defined")));
            }
        }
    }

    let server_trait = format_ident!("{}Server", trait_def.ident);
    let vis = &trait_def.vis;
    let server_items: Vec<TokenStream2> = items.iter().map(server_item).collect();
    let server_doc = format!(
        "What a Pimble server implements: `{}`'s methods and subscriptions, reached through \
         `pimble_rpc::dispatch` from any transport (generated by `#[pimble_api]`).",
        trait_def.ident
    );

    let method_table = items.iter().map(|item| {
        let name = &item.name;
        match &item.kind {
            Kind::Method => quote! { crate::dispatch::ApiMethod { name: #name, kind: crate::dispatch::MethodKind::Call } },
            Kind::Subscription { notification, unsubscribe } => quote! {
                crate::dispatch::ApiMethod {
                    name: #name,
                    kind: crate::dispatch::MethodKind::Subscription { notification: #notification, unsubscribe: #unsubscribe },
                }
            },
        }
    });

    let call_arms = items.iter().filter(|i| matches!(i.kind, Kind::Method)).map(|item| {
        let name = &item.name;
        let ident = &item.func.sig.ident;
        let (parsing, seq) = params_decoding(&item.args, quote! { return ::core::option::Option::Some(::core::result::Result::Err(e)); });
        let ext = item.with_extensions.then(|| quote! { ext, });
        quote! {
            #name => {
                #parsing
                ::core::option::Option::Some(crate::dispatch::answer(server.#ident(#ext #seq).await))
            }
        }
    });

    let subscribe_arms = items.iter().filter(|i| matches!(i.kind, Kind::Subscription { .. })).map(|item| {
        let name = &item.name;
        let ident = &item.func.sig.ident;
        // jsonrpsee rejects a subscription whose params do not decode and
        // sends no close notification after it.
        let (parsing, seq) = params_decoding(
            &item.args,
            quote! {
                pending.reject(e).await;
                return ::core::option::Option::Some(::core::result::Result::Ok(()));
            },
        );
        let ext = item.with_extensions.then(|| quote! { ext, });
        quote! {
            #name => {
                #parsing
                ::core::option::Option::Some(server.#ident(pending, #ext #seq).await)
            }
        }
    });

    let client_trait = {
        let ItemTrait { attrs, vis, unsafety, auto_token, trait_token, ident, generics, colon_token, supertraits, items, .. } = &trait_def;
        let ns = LitStr::new(namespace, Span::call_site());
        quote! {
            #[::jsonrpsee::proc_macros::rpc(client, namespace = #ns)]
            #(#attrs)*
            #vis #unsafety #auto_token #trait_token #ident #generics #colon_token #supertraits {
                #(#items)*
            }
        }
    };

    Ok(quote! {
        #client_trait

        #[cfg(not(target_arch = "wasm32"))]
        #[doc = #server_doc]
        #[::async_trait::async_trait]
        #vis trait #server_trait: Sized + Send + Sync + 'static {
            #(#server_items)*

            /// This server as a jsonrpsee `RpcModule`, for jsonrpsee's
            /// HTTP and WebSocket server (and tests that call it through
            /// `RpcModule::call`): every method of the trait, each reaching
            /// the server through the same dispatch as any other transport.
            fn into_rpc(self) -> ::jsonrpsee::RpcModule<Self> {
                crate::dispatch::native::into_rpc(self)
            }
        }

        // In a browser's worker nothing needs the server to be `Send`.
        #[cfg(target_arch = "wasm32")]
        #[doc = #server_doc]
        #[::async_trait::async_trait(?Send)]
        #vis trait #server_trait: Sized + 'static {
            #(#server_items)*
        }

        /// Every method and subscription of the API, by wire name: the one
        /// list, generated from the trait.
        pub const METHODS: &[crate::dispatch::ApiMethod] = &[#(#method_table),*];

        /// Answer the method `method` (a wire name) with `params`. `None`:
        /// no method of the API is called that (a subscription is not a
        /// method; see [`__subscribe`]).
        #[doc(hidden)]
        #[allow(unused_variables, unused_mut, clippy::let_unit_value)]
        pub async fn __call<S: #server_trait>(
            server: &S,
            ext: &crate::dispatch::Extensions,
            method: &str,
            params: ::jsonrpsee::types::Params<'_>,
        ) -> ::core::option::Option<::core::result::Result<::std::boxed::Box<::serde_json::value::RawValue>, ::jsonrpsee::types::ErrorObjectOwned>> {
            match method {
                #(#call_arms)*
                _ => ::core::option::Option::None,
            }
        }

        /// Start the subscription `method` (a wire name) with `params`; the
        /// server decides through `pending`. `None`: no subscription of the
        /// API is called that (and `pending` is dropped undecided).
        #[doc(hidden)]
        #[allow(unused_variables, unused_mut, clippy::let_unit_value)]
        pub async fn __subscribe<S: #server_trait>(
            server: &S,
            ext: &crate::dispatch::Extensions,
            method: &str,
            params: ::jsonrpsee::types::Params<'_>,
            pending: crate::dispatch::PendingSubscriptionSink,
        ) -> ::core::option::Option<crate::dispatch::SubscriptionResult> {
            match method {
                #(#subscribe_arms)*
                _ => ::core::option::Option::None,
            }
        }
    })
}

fn server_item(item: &Item) -> TokenStream2 {
    let mut sig = item.func.sig.clone();
    match item.kind {
        Kind::Method => {
            if item.with_extensions {
                sig.inputs.insert(1, syn::parse_quote!(ext: &crate::dispatch::Extensions));
            }
        }
        Kind::Subscription { .. } => {
            sig.inputs.insert(1, syn::parse_quote!(pending: crate::dispatch::PendingSubscriptionSink));
            if item.with_extensions {
                sig.inputs.insert(2, syn::parse_quote!(ext: &crate::dispatch::Extensions));
            }
            sig.output = ReturnType::Type(Default::default(), Box::new(syn::parse_quote!(crate::dispatch::SubscriptionResult)));
        }
    }
    let docs = &item.docs;
    quote! {
        #(#docs)*
        #sig;
    }
}

fn is_option(ty: &Type) -> bool {
    match ty {
        Type::Path(path) => path.path.segments.last().is_some_and(|s| s.ident == "Option"),
        _ => false,
    }
}

/// jsonrpsee-proc-macros 0.24's `render_params_decoding`, over the same
/// `jsonrpsee_types::Params`: what a call answers when its params do not
/// decode is the same error, word for word.
fn params_decoding(args: &[Arg], on_error: TokenStream2) -> (TokenStream2, TokenStream2) {
    if args.is_empty() {
        return (TokenStream2::new(), TokenStream2::new());
    }
    let idents: Vec<&Ident> = args.iter().map(|a| &a.ident).collect();
    let tys: Vec<&Type> = args.iter().map(|a| &a.ty).collect();
    let fields_seq = quote! { #(#idents),* };

    let decode_array = {
        let fields = args.iter().map(|Arg { ident, ty }| {
            let next = if is_option(ty) { quote!(optional_next) } else { quote!(next) };
            let ident_str = ident.to_string();
            quote! {
                let #ident: #ty = match seq.#next() {
                    ::core::result::Result::Ok(v) => v,
                    ::core::result::Result::Err(e) => {
                        ::tracing::debug!("Error parsing {} as {}: {:?}", #ident_str, stringify!(#ty), e);
                        #on_error
                    }
                };
            }
        });
        quote! {
            let mut seq = params.sequence();
            #(#fields)*
            (#fields_seq)
        }
    };

    let decode_map = {
        let generics: Vec<Ident> = (0..args.len()).map(|n| format_ident!("G{}", n)).collect();
        let fields = args.iter().zip(&generics).map(|(arg, g)| {
            let ident = &arg.ident;
            let name = ident.to_string();
            let snake = name.to_snake_case();
            let camel = name.to_lower_camel_case();
            quote! {
                #[serde(alias = #snake, alias = #camel)]
                #[serde(rename = #name)]
                #ident: #g,
            }
        });
        quote! {
            #[derive(::serde::Deserialize)]
            struct ParamsObject<#(#generics,)*> {
                #(#fields)*
            }
            let parsed: ParamsObject<#(#tys,)*> = match params.parse() {
                ::core::result::Result::Ok(p) => p,
                ::core::result::Result::Err(e) => {
                    ::tracing::debug!("Failed to parse JSON-RPC params as object: {:?}", e);
                    #on_error
                }
            };
            (#(parsed.#idents),*)
        }
    };

    let parsing = quote! {
        let (#fields_seq) = if params.is_object() {
            #decode_map
        } else {
            #decode_array
        };
    };
    (parsing, fields_seq)
}
