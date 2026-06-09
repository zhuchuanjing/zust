use anyhow::Result;
use dynamic::Type;

fn main() -> Result<()> {
    let vm = vm::Vm::with_all()?;
    vm.jit.write().unwrap().import_code(
        "vm_bug_repro",
        br#"
        pub struct Inner {
            value: i64,
        }

        pub struct RoleMini {
            inner: Inner,
            hp: i64,
        }

        pub struct TeamMini {
            role: RoleMini,
        }

        pub struct BigSummary {
            winner: i64,
            loser: i64,
        }

        pub fn make_big_direct() {
            BigSummary{winner: 1, loser: 2}
        }

        pub fn read_big_summary_direct() {
            make_big_direct().winner
        }

        pub fn make_big_with_arg(value: i64) {
            BigSummary{winner: value, loser: 0}
        }

        pub fn read_big_with_arg() {
            make_big_with_arg(7).winner
        }

        pub fn make_big_with_nested_while(value: i64) {
            let i = 0;
            let total = 0;
            while i < value {
                total = total + 1;
                i = i + 1;
            }
            BigSummary{winner: total, loser: 0}
        }

        pub fn read_big_with_nested_while() {
            make_big_with_nested_while(3).winner
        }

        pub fn make_big_with_team(team: TeamMini) {
            let score = team.role.inner.value;
            BigSummary{winner: score, loser: 0}
        }

        pub fn read_team_winner_direct() {
            let team = TeamMini{role: RoleMini{inner: Inner{value: 9}, hp: 1}};
            make_big_with_team(team).winner
        }

        pub fn read_team_winner_bound() {
            let team = TeamMini{role: RoleMini{inner: Inner{value: 9}, hp: 1}};
            let summary = make_big_with_team(team);
            summary.winner
        }

        pub fn read_team_winner_direct_arg(team: TeamMini) {
            make_big_with_team(team).winner
        }

        pub fn read_team_winner_bound_arg(team: TeamMini) {
            let summary = make_big_with_team(team);
            summary.winner
        }
        "#
        .to_vec(),
    )?;

    run_i64(&vm, "read_big_summary_direct", &[])?;
    run_i64(&vm, "read_big_with_arg", &[])?;
    run_i64(&vm, "read_big_with_nested_while", &[])?;
    run_i64(&vm, "read_team_winner_direct", &[])?;
    run_i64(&vm, "read_team_winner_bound", &[])?;

    let team_ty = vm.jit.write().unwrap().get_type("vm_bug_repro::read_team_winner_direct_arg", &[]).unwrap_or(Type::Any);
    println!("read_team_winner_direct_arg infer with no args: {team_ty:?}");
    let team_ty = vm.jit.write().unwrap().get_type("TeamMini", &[])?;
    compile_only(&vm, "read_team_winner_direct_arg", &[team_ty.clone()]);
    compile_only(&vm, "read_team_winner_bound_arg", &[team_ty]);

    Ok(())
}

fn run_i64(vm: &vm::Vm, name: &str, arg_tys: &[Type]) -> Result<()> {
    let full = format!("vm_bug_repro::{name}");
    print!("{name:<34}");
    match vm.jit.write().unwrap().get_fn_ptr(&full, arg_tys) {
        Ok((ptr, ret)) => {
            println!("compiled ret={:?}", ret);
            if ret == Type::I64 {
                let run: extern "C" fn() -> i64 = unsafe { std::mem::transmute(ptr) };
                println!("{name:<34}result={}", run());
            }
        }
        Err(err) => {
            println!("FAILED: {err:#}");
        }
    }
    Ok(())
}

fn compile_only(vm: &vm::Vm, name: &str, arg_tys: &[Type]) {
    let full = format!("vm_bug_repro::{name}");
    print!("{name:<34}");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| vm.jit.write().unwrap().get_fn_ptr(&full, arg_tys)));
    match result {
        Ok(Ok((_ptr, ret))) => println!("compiled ret={:?}", ret),
        Ok(Err(err)) => println!("FAILED: {err:#}"),
        Err(panic) => {
            if let Some(message) = panic.downcast_ref::<String>() {
                println!("PANIC: {message}");
            } else if let Some(message) = panic.downcast_ref::<&str>() {
                println!("PANIC: {message}");
            } else {
                println!("PANIC");
            }
        }
    }
}
