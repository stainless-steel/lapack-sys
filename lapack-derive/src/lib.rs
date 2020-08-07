use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2, TokenTree};
use quote::quote;

type Args = syn::punctuated::Punctuated<syn::FnArg, syn::token::Comma>;
type Call = syn::punctuated::Punctuated<syn::Expr, syn::token::Comma>;

#[proc_macro_attribute]
pub fn lapack(_attr: TokenStream, func: TokenStream) -> TokenStream {
    lapack2(syn::parse(func).unwrap()).into()
}

/// TokenStream2-based main routine
fn lapack2(func: TokenStream2) -> TokenStream2 {
    let f = parse_foreign_fn(&func);
    let wrap = wrap(&f);
    quote! {
        #func
        #wrap
    }
}

/// extern "C" { fn dgetrs_(...); } -> fn dgetrs_(...);
fn parse_foreign_fn(func: &TokenStream2) -> syn::ForeignItemFn {
    let func = if let Some(func) = func.clone().into_iter().skip(2 /* 'extern', 'C' */).next() {
        if let TokenTree::Group(group) = func {
            group.stream()
        } else {
            unreachable!("#[lapack] attribute must be put to `extern \"C\"` block")
        }
    } else {
        unreachable!("#[lapack] attribute must be put to `extern \"C\"` block")
    };
    syn::parse2(func).unwrap()
}

/// Generate token stream of wrapped function
fn wrap(f: &syn::ForeignItemFn) -> TokenStream2 {
    // like dgetrs_
    let lapack_sys_name = &f.sig.ident;
    // like dgetrs
    let lapack_name = lapack_sys_name
        .to_string()
        .trim_end_matches('_')
        .to_string();
    let lapack_name = syn::Ident::new(&lapack_name, Span::call_site());
    let input = signature_input(&f.sig.inputs);
    let call = call(&f.sig.inputs);
    let output = &f.sig.output;
    quote! {
        pub unsafe fn #lapack_name ( #input ) #output {
            #lapack_sys_name ( #call )
        }
    }
}

/// Pointer type `*const T` or `*mut T`
enum Ptr {
    Constant(String),
    Mutable(String),
}

impl Ptr {
    /// Get `T` as String
    fn ty(&self) -> String {
        match self {
            Ptr::Constant(ty) => ty.clone(),
            Ptr::Mutable(ty) => ty.clone(),
        }
    }
}

impl From<syn::TypePtr> for Ptr {
    fn from(ptr_ty: syn::TypePtr) -> Self {
        match &*ptr_ty.elem {
            syn::Type::Path(path) => {
                let path = quote! { #path }.to_string();
                match ptr_ty.mutability {
                    Some(_) => Ptr::Mutable(path),
                    None => Ptr::Constant(path),
                }
            }
            _ => unimplemented!("Pointer for non-path is not supported yet"),
        }
    }
}

/// Parse type ascription pattern `a: *mut f64` into ("a", "f64")
fn parse_input(pat: &syn::PatType) -> (String, Ptr) {
    let name = match &*pat.pat {
        syn::Pat::Ident(ident) => ident.ident.to_string(),
        _ => unreachable!(),
    };
    let ptr = match &*pat.ty {
        syn::Type::Ptr(ptr_ty) => ptr_ty.clone().into(),
        _ => unreachable!("LAPACK raw API must be consists of pointer arguments"),
    };
    (name, ptr)
}

/// Convert pointer-based raw-LAPACK API into value and reference based API
fn signature_input(args: &Args) -> Args {
    args.iter()
        .cloned()
        .map(|mut arg| {
            match &mut arg {
                syn::FnArg::Typed(arg) => {
                    let (name, ptr) = parse_input(&arg);
                    let new_type = match name.to_lowercase().as_str() {
                        // pointer -> mutable reference
                        "info" => "&mut i32".into(),
                        // pointer -> array
                        "a" | "b" | "ipiv" | "work" => match ptr {
                            Ptr::Constant(ty) => format!("&[{}]", ty),
                            Ptr::Mutable(ty) => format!("&mut [{}]", ty),
                        },
                        // pointer -> value
                        _ => ptr.ty(),
                    };
                    *arg.ty = syn::parse_str(&new_type).unwrap();
                }
                _ => unreachable!("LAPACK raw API does not contains non-typed argument"),
            }
            arg
        })
        .collect()
}

fn call(args: &Args) -> Call {
    args.iter()
        .map(|arg| match arg {
            syn::FnArg::Typed(arg) => {
                let (name, ptr) = parse_input(arg);
                let expr = match name.to_lowercase().as_str() {
                    "info" => "info".into(),
                    "a" | "b" | "ipiv" | "work" => match ptr {
                        Ptr::Constant(_) => format!("{}.as_ptr()", name),
                        Ptr::Mutable(_) => format!("{}.as_mut_ptr()", name),
                    },
                    _ => match ptr {
                        Ptr::Constant(ty) => match ty.as_str() {
                            "u8" => format!("&({} as c_char)", name),
                            _ => format!("&{}", name),
                        },
                        Ptr::Mutable(ty) => match ty.as_str() {
                            "u8" => format!("&mut ({} as c_char)", name),
                            _ => format!("&mut {}", name),
                        },
                    },
                };
                syn::parse_str::<syn::Expr>(&expr).unwrap()
            }
            _ => unreachable!(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_input() {
        let dgetrs = r#"
        pub fn dgetrs_(
            trans: *const c_char,
            n: *const c_int,
            nrhs: *const c_int,
            A: *const f64,
            lda: *const c_int,
            ipiv: *const c_int,
            B: *mut f64,
            ldb: *const c_int,
            info: *mut c_int,
        );
        "#;
        let f: syn::ForeignItemFn = syn::parse_str(dgetrs).unwrap();
        let result = super::signature_input(&f.sig.inputs);
        let result_str = quote! { #result }.to_string();
        let answer: TokenStream2 = syn::parse_str(
            r#"
            trans: u8,
            n: i32,
            nrhs: i32,
            A: &[f64],
            lda: i32,
            ipiv: &[i32],
            B: &mut [f64],
            ldb: i32,
            info: &mut i32
            "#,
        )
        .unwrap();
        assert_eq!(result_str, answer.to_string());
    }

    #[test]
    fn call() {
        let dgetrs = r#"
        pub fn dgetrs_(
            trans: *const c_char,
            n: *const c_int,
            nrhs: *const c_int,
            A: *const f64,
            lda: *const c_int,
            ipiv: *const c_int,
            B: *mut f64,
            ldb: *const c_int,
            info: *mut c_int,
        );
        "#;
        let f: syn::ForeignItemFn = syn::parse_str(dgetrs).unwrap();
        let result = super::call(&f.sig.inputs);
        let result_str = quote! { #result }.to_string();
        let answer: TokenStream2 = syn::parse_str(
            r#"
            &(trans as c_char),
            &n,
            &nrhs,
            A.as_ptr(),
            &lda,
            ipiv.as_ptr(),
            B.as_mut_ptr(),
            &ldb,
            info
            "#,
        )
        .unwrap();
        assert_eq!(result_str, answer.to_string());
    }

    #[test]
    fn wrap_dgetrs() {
        let dgetrs = r#"
        pub fn dgetrs_(
            trans: *const c_char,
            n: *const c_int,
            nrhs: *const c_int,
            A: *const f64,
            lda: *const c_int,
            ipiv: *const c_int,
            B: *mut f64,
            ldb: *const c_int,
            info: *mut c_int,
        );
        "#;
        let wrapped = super::wrap(&syn::parse_str(dgetrs).unwrap());
        let expected = r#"
        pub unsafe fn dgetrs(
            trans: u8,
            n: i32,
            nrhs: i32,
            A: &[f64],
            lda: i32,
            ipiv: &[i32],
            B: &mut [f64],
            ldb: i32,
            info: &mut i32
        ) {
            dgetrs_(
                &(trans as c_char),
                &n,
                &nrhs,
                A.as_ptr(),
                &lda,
                ipiv.as_ptr(),
                B.as_mut_ptr(),
                &ldb,
                info
            )
        }
        "#;
        let expected: TokenStream2 = syn::parse_str(expected).unwrap();
        assert_eq!(wrapped.to_string(), expected.to_string());
    }

    /// Test for return value case
    #[test]
    fn wrap_dlange() {
        let dgetrs = r#"
        pub fn dlange_(
            norm: *const c_char,
            m: *const c_int,
            n: *const c_int,
            A: *const f64,
            lda: *const c_int,
            work: *mut f64,
        ) -> f64;
        "#;
        let wrapped = super::wrap(&syn::parse_str(dgetrs).unwrap());
        let expected = r#"
        pub unsafe fn dlange(
            norm: u8,
            m: i32,
            n: i32,
            A: &[f64],
            lda: i32,
            work: &mut [f64]
        ) -> f64 {
            dlange_(
                &(norm as c_char),
                &m,
                &n,
                A.as_ptr(),
                &lda,
                work.as_mut_ptr()
            )
        }
        "#;
        let expected: TokenStream2 = syn::parse_str(expected).unwrap();
        assert_eq!(wrapped.to_string(), expected.to_string());
    }
}