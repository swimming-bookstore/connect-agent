//! Box-side agent pieces. The plane lives in connect-control-plane.

pub mod ai;
pub mod chrome;
pub mod code;
pub mod jpeg;
pub mod plane;
pub mod protocol;
pub mod rtc;
pub mod shell;
pub mod video;

pub mod pb {
    tonic::include_proto!("connect");
}
