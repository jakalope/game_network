extern crate serde;
#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate log;
extern crate bincode;
extern crate bidir_map;
extern crate spmc;

#[macro_use]
mod msg;

mod bitvec;
mod control;
pub mod server;
pub mod client;
