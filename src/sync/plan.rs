//! The reconciliation rule shared by `pull`, `status`, and `update`.
//!
//! For one path there are three versions: the remote (`R`), the baseline last applied (`B`),
//! and the local working copy (`L`), each reduced to a Git blob SHA or absent. Everything the
//! sync commands decide follows from comparing them.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    None,
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Unchanged,
    LocalOnly,
    RemoteOnly,
    Conflict,
    /// Removed locally; the remote is unchanged.
    Deleted,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Unchanged => "unchanged",
            State::LocalOnly => "local-only",
            State::RemoteOnly => "remote-only",
            State::Conflict => "conflict",
            State::Deleted => "deleted",
        }
    }
}

impl Change {
    pub fn as_str(self) -> &'static str {
        match self {
            Change::None => "none",
            Change::Added => "added",
            Change::Modified => "modified",
            Change::Deleted => "deleted",
        }
    }
}

fn change(base: Option<&str>, current: Option<&str>) -> Change {
    match (base, current) {
        (None, None) => Change::None,
        (None, Some(_)) => Change::Added,
        (Some(_), None) => Change::Deleted,
        (Some(b), Some(c)) if b == c => Change::None,
        (Some(_), Some(_)) => Change::Modified,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    pub local: Change,
    pub remote: Change,
    pub state: State,
    /// Both sides changed but ended up identical; the baseline only needs to catch up.
    pub converged: bool,
}

pub fn classify(remote: Option<&str>, base: Option<&str>, local: Option<&str>) -> Verdict {
    let local_change = change(base, local);
    let remote_change = change(base, remote);
    let converged =
        local_change != Change::None && remote_change != Change::None && remote == local;
    let state = if converged {
        State::Unchanged
    } else {
        match (local_change, remote_change) {
            (Change::None, Change::None) => State::Unchanged,
            (Change::Deleted, Change::None) => State::Deleted,
            (_, Change::None) => State::LocalOnly,
            (Change::None, _) => State::RemoteOnly,
            _ => State::Conflict,
        }
    };
    Verdict {
        local: local_change,
        remote: remote_change,
        state,
        converged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Option<&str> = Some("a");
    const B: Option<&str> = Some("b");
    const C: Option<&str> = Some("c");

    fn state(remote: Option<&str>, base: Option<&str>, local: Option<&str>) -> State {
        classify(remote, base, local).state
    }

    #[test]
    fn matrix_of_local_and_remote_changes() {
        // everything equal / nothing anywhere
        assert_eq!(state(A, A, A), State::Unchanged);
        assert_eq!(state(None, None, None), State::Unchanged);
        // only local changed
        assert_eq!(state(A, A, B), State::LocalOnly);
        assert_eq!(state(None, None, A), State::LocalOnly);
        assert_eq!(state(A, A, None), State::Deleted);
        // only remote changed
        assert_eq!(state(B, A, A), State::RemoteOnly);
        assert_eq!(state(A, None, None), State::RemoteOnly);
        assert_eq!(state(None, A, A), State::RemoteOnly);
        // both changed differently
        assert_eq!(state(B, A, C), State::Conflict);
        assert_eq!(state(A, None, B), State::Conflict); // remote add collides with local untracked file
        assert_eq!(state(None, A, B), State::Conflict); // remote delete over a local edit
        assert_eq!(state(B, A, None), State::Conflict); // remote edit over a local delete
    }

    #[test]
    fn identical_results_converge() {
        for (r, b, l) in [(B, A, B), (A, None, A), (None, A, None)] {
            let v = classify(r, b, l);
            assert_eq!(v.state, State::Unchanged);
            assert!(v.converged);
        }
    }
}
