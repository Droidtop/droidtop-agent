//! `pair`: pairing with a handheld or another computer.

use std::sync::Arc;

use droidtop_agent_pair as pair;
use droidtop_agent_service::Agent;

pub fn pair(agent: &Arc<Agent>, args: &[&str]) -> Result<(), String> {
    let done = |name: String| println!("Paired with {name}.");
    match args {
        ["compare"] => pair::compare_offer(&agent.store).map(done),
        ["find"] => pair::compare_find(&agent.store).map(done),
        [] => pair::show(&agent.store).map(done),
        code => pair::pair(&agent.store, &code.join(" ")).map(done),
    }
}
