// Copyright 2017 Parity Technologies (UK) Ltd.
// This file is part of Substrate.

// Substrate is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Substrate is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Substrate.  If not, see <http://www.gnu.org/licenses/>.

//! The Substrate runtime. This can be compiled with #[no_std], ready for Wasm.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate substrate_runtime_std as rstd;
extern crate substrate_codec as codec;
extern crate substrate_runtime_primitives as runtime_primitives;

#[cfg(feature = "std")]
extern crate serde;

#[cfg(feature = "std")]
#[macro_use]
extern crate serde_derive;

#[macro_use]
extern crate substrate_runtime_support as runtime_support;

#[cfg(test)]
#[macro_use]
extern crate hex_literal;
#[cfg(test)]
extern crate ed25519;
#[cfg(test)]
extern crate substrate_keyring as keyring;
#[cfg_attr(test, macro_use)]
extern crate substrate_primitives as primitives;
#[macro_use]
extern crate substrate_runtime_io as runtime_io;
#[macro_use]
extern crate substrate_runtime_version as runtime_version;


#[cfg(feature = "std")] pub mod genesismap;
pub mod system;

use rstd::prelude::*;
use codec::Slicable;

use runtime_primitives::traits::{BlindCheckable, BlakeTwo256};
use runtime_primitives::Ed25519Signature;
use runtime_version::RuntimeVersion;
pub use primitives::hash::H256;

/// Test runtime version.
pub const VERSION: RuntimeVersion = RuntimeVersion {
	spec_name: ver_str!("test"),
	impl_name: ver_str!("parity-test"),
	authoring_version: 1,
	spec_version: 1,
	impl_version: 1,
};

fn version() -> RuntimeVersion {
	VERSION
}

/// Calls in transactions.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(Debug, Serialize, Deserialize))]
pub struct Transfer {
	pub from: AccountId,
	pub to: AccountId,
	pub amount: u64,
	pub nonce: u64,
}

impl Slicable for Transfer {
	fn encode(&self) -> Vec<u8> {
		let mut v = Vec::new();
		self.from.using_encoded(|s| v.extend(s));
		self.to.using_encoded(|s| v.extend(s));
		self.amount.using_encoded(|s| v.extend(s));
		self.nonce.using_encoded(|s| v.extend(s));
		v
	}

	fn decode<I: ::codec::Input>(input: &mut I) -> Option<Self> {
		Slicable::decode(input).map(|(from, to, amount, nonce)| Transfer { from, to, amount, nonce })
	}
}

/// Extrinsic for test-runtime.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(Debug, Serialize, Deserialize))]
pub struct Extrinsic {
	pub transfer: Transfer,
	pub signature: Ed25519Signature,
}

impl Slicable for Extrinsic {
	fn encode(&self) -> Vec<u8> {
		let mut v = Vec::new();
		self.transfer.using_encoded(|s| v.extend(s));
		self.signature.using_encoded(|s| v.extend(s));
		v
	}

	fn decode<I: ::codec::Input>(input: &mut I) -> Option<Self> {
		Slicable::decode(input).map(|(transfer, signature)| Extrinsic { transfer, signature })
	}
}

impl BlindCheckable for Extrinsic {
	type Checked = Self;
	type Address = AccountId;

	fn sender(&self) -> &Self::Address {
		&self.transfer.from
	}
	fn check(self) -> Result<Self, &'static str> {
		if ::runtime_primitives::verify_encoded_lazy(&self.signature, &self.transfer, &self.transfer.from) {
			Ok(self)
		} else {
			Err("bad signature")
		}
	}
}

/// An identifier for an account on this system.
pub type AccountId = H256;
/// A simple hash type for all our hashing.
pub type Hash = H256;
/// The block number type used in this runtime.
pub type BlockNumber = u64;
/// Index of a transaction.
pub type Index = u64;
/// The digest of a block.
pub type Digest = runtime_primitives::generic::Digest<Vec<u8>>;
/// A test block.
pub type Block = runtime_primitives::generic::Block<Header, Extrinsic>;
/// A test block's header.
pub type Header = runtime_primitives::generic::Header<BlockNumber, BlakeTwo256, Vec<u8>>;

/// Run whatever tests we have.
pub fn run_tests(mut input: &[u8]) -> Vec<u8> {
	use runtime_io::print;

	print("run_tests...");
	let block = Block::decode(&mut input).unwrap();
	print("deserialised block.");
	let stxs = block.extrinsics.iter().map(Slicable::encode).collect::<Vec<_>>();
	print("reserialised transactions.");
	[stxs.len() as u8].encode()
}

pub mod api {
	use system;
	impl_stubs!(
		version => |()| super::version(),
		authorities => |()| system::authorities(),
		initialise_block => |header| system::initialise_block(header),
		execute_block => |block| system::execute_block(block),
		apply_extrinsic => |utx| system::execute_transaction(utx),
		finalise_block => |()| system::finalise_block()
	);
}
