// Copyright 2019 Parity Technologies (UK) Ltd.
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

//! A set of constant values used in substrate runtime.

/// Money matters.
pub mod currency {
	use hb_node_primitives::Balance;

	pub const MILLICENTS: Balance = 1_000_000_000;
	pub const CENTS: Balance = 1_000 * MILLICENTS; // assume this is worth about a cent.
	pub const DOLLARS: Balance = 100 * CENTS;
}

/// Time.
pub mod time {
	use hb_node_primitives::{BlockNumber, Moment};

	pub const MILLISECS_PER_BLOCK: Moment = 6000;
	pub const SECS_PER_BLOCK: Moment = MILLISECS_PER_BLOCK / 1000;

	pub const SLOT_DURATION: Moment = 1650;

	pub const EPOCH_DURATION_IN_BLOCKS: BlockNumber = 10 * MINUTES;
	pub const EPOCH_DURATION_IN_SLOTS: u64 = {
		const SLOT_FILL_RATE: f64 = MILLISECS_PER_BLOCK as f64 / SLOT_DURATION as f64;

		(EPOCH_DURATION_IN_BLOCKS as f64 * SLOT_FILL_RATE) as u64
	};

	// These time units are defined in number of blocks.
	pub const MINUTES: BlockNumber = 60 / (SECS_PER_BLOCK as BlockNumber);
	pub const HOURS: BlockNumber = MINUTES * 60;
	pub const DAYS: BlockNumber = HOURS * 24;
}

// CRITICAL NOTE: The system module maintains two constants: a _maximum_ block weight and a
// _ratio_ of it yielding the portion which is accessible to normal transactions (reserving the rest
// for operational ones). `TARGET_BLOCK_FULLNESS` is entirely independent and the system module is
// not aware of if, nor should it care about it. This constant simply denotes on which ratio of the
// _maximum_ block weight we tweak the fees. It does NOT care about the type of the dispatch.
//
// For the system to be configured in a sane way, `TARGET_BLOCK_FULLNESS` should always be less than
// the ratio that `system` module uses to find normal transaction quota.
/// Fee-related.
pub mod fee {
	pub use sp_runtime::Perbill;

	/// The block saturation level. Fees will be updates based on this value.
	pub const TARGET_BLOCK_FULLNESS: Perbill = Perbill::from_percent(25);
}
