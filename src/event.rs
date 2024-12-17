use std::time::Instant;

#[derive(Debug)]
pub enum UserEvent {
    RequestRepaint {
        when: Instant,
        cumulative_pass_nr: u64,
    },
}

pub enum EventResult {
    Wait,
    RepaintNow,
    RepaintNext,
    RepaintAt(Instant),
    Exit,
}
