#![recursion_limit="128"]

extern crate proc_macro;
extern crate syn;
#[macro_use]
extern crate quote;

use proc_macro::TokenStream;

#[proc_macro_derive(AudioScheduledSourceNode)]
pub fn audio_scheduled_source_node(input: TokenStream) -> TokenStream {
    let ast: syn::DeriveInput = syn::parse(input).unwrap();
    let gen = impl_audio_scheduled_source_node(&ast);
    gen.into()
}

fn impl_audio_scheduled_source_node(ast: &syn::DeriveInput) -> quote::Tokens {
    let name = &ast.ident;
    quote! {
        impl #name {
            fn should_play_at(&self, tick: Tick) -> (bool, bool) {
                if self.start_at.is_none() {
                    return (false, true);
                }

                if tick < self.start_at.unwrap() {
                    (false, false)
                } else {
                    if let Some(stop_at) = self.stop_at {
                        if tick >= stop_at {
                            return (false, true);
                        }
                    }
                    (true, false)
                }
            }

            fn start(&mut self, tick: Tick) -> bool {
                // We can only allow a single call to `start` and always before
                // any `stop` calls.
                if self.start_at.is_some() || self.stop_at.is_some() {
                    return false;
                }
                self.start_at = Some(tick);
                true
            }

            fn stop(&mut self, tick: Tick) -> bool {
                // We can only allow calls to `stop` after `start` is called.
                if self.start_at.is_none() {
                    return false;
                }
                // If `stop` is called again after already having been called,
                // the last invocation will be the only one applied.
                self.stop_at = Some(tick);
                true
            }

            fn maybe_trigger_onended_callback(&mut self) {
                // We cannot have an end without a start.
                if self.start_at.is_none() || self.onended_callback.is_none() {
                    return;
                }
                let callback = self.onended_callback.take().unwrap();
                let mut callback = callback.0.lock().unwrap();
                callback.take().unwrap()();
            }

            fn handle_source_node_message(&mut self, message: AudioScheduledSourceNodeMessage, sample_rate: f32) {
                match message {
                    AudioScheduledSourceNodeMessage::Start(when) => {
                        self.start(Tick::from_time(when, sample_rate));
                    }
                    AudioScheduledSourceNodeMessage::Stop(when) => {
                        self.stop(Tick::from_time(when, sample_rate));
                    }
                    AudioScheduledSourceNodeMessage::RegisterOnEndedCallback(callback) => {
                        self.onended_callback = Some(callback);
                    }
                }
            }
        }
    }
}

#[proc_macro_derive(AudioNodeCommon)]
pub fn channel_info(input: TokenStream) -> TokenStream {

    let ast: syn::DeriveInput = syn::parse(input).unwrap();
    let name = &ast.ident;
    let gen = quote! {
        impl ::node::AudioNodeCommon for #name {
            fn channel_info(&self) -> &::node::ChannelInfo {
                &self.channel_info
            }

            fn channel_info_mut(&mut self) -> &mut ::node::ChannelInfo {
                &mut self.channel_info
            }
        }
    };
    gen.into()
}
