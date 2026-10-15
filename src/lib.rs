//! TokenVesting: a SilverScript contract that holds tokens of the KCC-20 reference and
//! releases them to one beneficiary on a schedule, and the checks of it on the script
//! engine and the transaction rules of a Kaspa node.
//!
//! educational / unaudited / not for real funds

pub mod contract;
pub mod judge;
pub mod launch;
pub mod release;
