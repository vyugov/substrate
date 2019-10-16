#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;
use client::decl_runtime_apis;

decl_runtime_apis! {

	
	pub trait HbbftApi {
		fn do_nothing();
	}
}
