use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, Data, Fields};

#[proc_macro_derive(FromBuilder, attributes(builder))]
pub fn from_builder_derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let struct_name = &input.ident;

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => panic!("FromBuilder can only be derived for structs with named fields"),
        },
        _ => panic!("FromBuilder can only be derived for structs"),
    };

    let builder_calls = fields.iter().map(|field| {
        let field_name = &field.ident;
        let field_type = &field.ty;

        quote! {
            #field_name:
                builder
                    .object::<gtk::glib::Object>(stringify!(#field_name))
                    .expect(&format!("Failed to get object: {}", stringify!(#field_name)))
                    .downcast::<#field_type>()
                    .expect(&format!("Failed to downcast object: {}", stringify!(#field_name)))
        }
    });

    let expanded = quote! {
        impl #struct_name {
            pub fn from_builder(builder: &gtk::Builder) -> Self {
                Self {
                    #(#builder_calls),*
                }
            }

            pub fn from_builder_str(builder_str: &str) -> Self {
                Self::from_builder(&gtk::Builder::from_string(builder_str))
            }
        }
    };

    TokenStream::from(expanded)
}
