use token_vesting::contract::{Compiled, VestingState};

#[test]
fn the_contract_compiles() {
    let state = VestingState {
        beneficiary: [1; 32],
        token_covid: [2; 32],
        token_template: [3; 32],
        start: 1000,
        cliff: 10,
        duration: 100,
        total: 1000,
        released: 0,
    };
    let compiled = Compiled::new(&state);
    println!(
        "bytecode {} bytes, {} steps",
        compiled.bytecode.len(),
        compiled.steps.len()
    );
}
