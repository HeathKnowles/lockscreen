//! Generated protobuf/gRPC types, produced by `build.rs` into OUT_DIR.
//!
//! Both protos share package `phoned.v1`, so prost emits a single module
//! holding control and presence items.

tonic::include_proto!("phoned.v1");

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn lock_request_round_trip() {
        let req = LockRequest {
            reason: "walk-away".into(),
        };
        let bytes = req.encode_to_vec();
        let back = LockRequest::decode(&bytes[..]).unwrap();
        assert_eq!(back.reason, "walk-away");
    }

    #[test]
    fn watch_state_matches_lockscreen_state_machine() {
        // Must mirror lockscreen's waiting/watching/locked (watch.rs).
        assert_eq!(WatchState::Waiting as i32, 1);
        assert_eq!(WatchState::Watching as i32, 2);
        assert_eq!(WatchState::Locked as i32, 3);
    }

    #[test]
    fn codegen_emits_client_and_server() {
        // Compile-time proof that both stubs were generated for both services.
        type _ControlClient<C> = control_client::ControlClient<C>;
        type _ControlServer<S> = control_server::ControlServer<S>;
        type _PresenceClient<C> = presence_client::PresenceClient<C>;
        type _PresenceServer<S> = presence_server::PresenceServer<S>;
    }
}
