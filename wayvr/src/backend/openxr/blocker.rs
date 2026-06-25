use log::trace;
use wayvr_openxr_layer_common::{BlockMode, ControlWriter};

use crate::state::AppState;

pub(super) struct InputBlocker {
    control: ControlWriter,
    blocked_last_frame: [BlockMode; 2],
}

impl InputBlocker {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            control: ControlWriter::new()?,
            blocked_last_frame: [BlockMode::None; 2],
        })
    }

    pub fn unblock(&self) {
        self.control.clear();
    }

    pub fn update(&mut self, app: &mut AppState) {
        // Refresh the liveness heartbeat every frame so readers (including
        // sandboxed game processes in a different PID namespace) can tell this
        // writer is alive. If wayvr dies, the heartbeat goes stale and readers
        // fail open, never leaving game input stuck blocked.
        self.control.heartbeat();

        // Each hand is published separately: pointing one hand at an overlay
        // must leave the other hand's input untouched.
        let blocked = if app.session.config.block_game_input {
            [0, 1].map(|idx| app.input_state.pointers[idx].interaction.block_input)
        } else {
            [BlockMode::None; 2]
        };

        if blocked != self.blocked_last_frame {
            trace!("Input block: left={:?} right={:?}", blocked[0], blocked[1]);
            self.control.set(blocked[0], blocked[1]);
        }

        self.blocked_last_frame = blocked;
    }
}
