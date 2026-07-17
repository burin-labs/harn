use crate::cli::{PersonaArgs, PersonaCommand, PersonaSupervisionCommand};

pub(crate) async fn run(args: PersonaArgs) -> Result<(), String> {
    let PersonaArgs {
        command,
        manifest,
        state_dir,
    } = args;
    let manifest = manifest.as_deref();
    match command {
        PersonaCommand::New(args) => super::persona_scaffold::run_new(&args),
        PersonaCommand::Doctor(args) => super::persona_doctor::run_doctor(manifest, &args).await,
        PersonaCommand::Check(args) => {
            super::persona::run_check(manifest, &args);
            Ok(())
        }
        PersonaCommand::List(args) => {
            super::persona::run_list(manifest, &args);
            Ok(())
        }
        PersonaCommand::Inspect(args) => {
            super::persona::run_inspect(manifest, &args);
            Ok(())
        }
        PersonaCommand::Activate(args) => super::persona_activation::run_activate(manifest, &args),
        PersonaCommand::Deactivate(args) => {
            super::persona_activation::run_deactivate(manifest, &args)
        }
        PersonaCommand::Activations(args) => {
            super::persona_activation::run_activations(manifest, &args)
        }
        PersonaCommand::Status(args) => {
            super::persona::run_status(manifest, &state_dir, &args).await
        }
        PersonaCommand::Pause(args) => super::persona::run_pause(manifest, &state_dir, &args).await,
        PersonaCommand::Resume(args) => {
            super::persona::run_resume(manifest, &state_dir, &args).await
        }
        PersonaCommand::Disable(args) => {
            super::persona::run_disable(manifest, &state_dir, &args).await
        }
        PersonaCommand::Tick(args) => super::persona::run_tick(manifest, &state_dir, &args).await,
        PersonaCommand::Trigger(args) => {
            super::persona::run_trigger(manifest, &state_dir, &args).await
        }
        PersonaCommand::Spend(args) => super::persona::run_spend(manifest, &state_dir, &args).await,
        PersonaCommand::Supervision(args) => match args.command {
            PersonaSupervisionCommand::Tail(args) => {
                super::persona_supervision::run_tail(manifest, &state_dir, &args).await
            }
        },
    }
}
