//! cu-host: started by the supervisor, configured over stdin.

use std::io::Read;
use std::process::ExitCode;

use desktop_host::{
    HostConfig,
    model_bootstrap::{HostModelServices, ModelBootstrap, ModelPurpose},
    owned_document_bootstrap::OwnedDocumentBootstrap,
    shell,
    teacher_observation::{TeacherObservationWriter, open_inherited_fd4},
};
use desktop_ipc::link::{FramedReader, Link};
use interaction_contracts::desktop::handshake::PeerRole;
use interaction_contracts::desktop::message::Message;

type StartupBootstraps = (Option<OwnedDocumentBootstrap>, Option<ModelBootstrap>);
type StartupOptions = (StartupBootstraps, bool);

fn computer_image_arguments(args: &[String]) -> Result<(&[String], bool), String> {
    let requested = args.last().is_some_and(|arg| arg == "--computer-image");
    let prefix = if requested {
        &args[..args.len() - 1]
    } else {
        args
    };
    if prefix.iter().any(|arg| arg == "--computer-image")
        || (requested && !prefix.iter().any(|arg| arg == "--model-bootstrap"))
    {
        return Err("invalid private computer image arguments".into());
    }
    Ok((prefix, requested))
}

fn validate_computer_image_authorization(
    requested: bool,
    bootstrap: Option<&ModelBootstrap>,
    config: &HostConfig,
) -> Result<(), String> {
    let authorized =
        bootstrap.is_some_and(|value| value.authorizes(ModelPurpose::ComputerProposals));
    if requested != authorized
        || (requested
            && (config.native_window_scope.is_none() || config.native_target_bundle.is_none()))
    {
        return Err(
            "computer images require private computer_proposals and native window authorization"
                .into(),
        );
    }
    Ok(())
}

fn observation_arguments(args: &[String]) -> Result<(&[String], bool), String> {
    let requested = args
        .last()
        .is_some_and(|arg| arg == "--teacher-observation");
    let prefix = if requested {
        &args[..args.len() - 1]
    } else {
        args
    };
    if prefix.iter().any(|arg| arg == "--teacher-observation")
        || (requested && !prefix.iter().any(|arg| arg == "--model-bootstrap"))
    {
        return Err("invalid private teacher observation arguments".into());
    }
    Ok((prefix, requested))
}

fn validate_observation_authorization(
    requested: bool,
    bootstrap: Option<&ModelBootstrap>,
) -> Result<(), String> {
    if requested && !bootstrap.is_some_and(|value| value.authorizes(ModelPurpose::S1Teacher)) {
        return Err("teacher observation requires private s1_teacher authorization".into());
    }
    Ok(())
}

fn read_startup_bootstraps(
    args: &[String],
    stdin: &mut impl Read,
) -> Result<StartupOptions, String> {
    match args {
        [] => Ok(((None, None), false)),
        [arg] if arg == "--model-bootstrap" => {
            ModelBootstrap::read(stdin).map(|model| ((None, Some(model)), false))
        }
        [arg] if arg == "--owned-document-bootstrap" => {
            OwnedDocumentBootstrap::read(stdin).map(|owned| ((Some(owned), None), false))
        }
        [owned_arg, model_arg]
            if owned_arg == "--owned-document-bootstrap" && model_arg == "--model-bootstrap" =>
        {
            let owned = OwnedDocumentBootstrap::read(stdin)?;
            let model = ModelBootstrap::read(stdin)?;
            Ok(((Some(owned), Some(model)), false))
        }
        [owned_arg, revisit_arg]
            if owned_arg == "--owned-document-bootstrap"
                && revisit_arg == "--owned-revisit-control" =>
        {
            let owned = OwnedDocumentBootstrap::read(stdin)?;
            Ok(((Some(owned), None), true))
        }
        [owned_arg, model_arg, revisit_arg]
            if owned_arg == "--owned-document-bootstrap"
                && model_arg == "--model-bootstrap"
                && revisit_arg == "--owned-revisit-control" =>
        {
            let owned = OwnedDocumentBootstrap::read(stdin)?;
            let model = ModelBootstrap::read(stdin)?;
            Ok(((Some(owned), Some(model)), true))
        }
        _ => Err("unsupported startup arguments".into()),
    }
}

fn validate_revisit_control_authorization(
    requested: bool,
    config: &HostConfig,
) -> Result<(), String> {
    if requested
        && (!config.allow_owned_document_save || config.owned_document_registration.is_none())
    {
        Err("private owned revisit needs owned Save authorization".into())
    } else {
        Ok(())
    }
}

fn validate_owned_save_model_authorization(
    config: &HostConfig,
    model_bootstrap: Option<&ModelBootstrap>,
) -> Result<(), String> {
    if config.allow_owned_document_save
        && config.planning_backend == desktop_host::config::PlanningBackend::Model
        && !model_bootstrap.is_some_and(|bootstrap| bootstrap.authorizes(ModelPurpose::S2Planning))
    {
        return Err(
            "owned Save with model planning requires private s2_planning authorization".into(),
        );
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let (prefix_args, computer_image_requested) = match computer_image_arguments(&args) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("cu-host: {error}");
            return ExitCode::from(2);
        }
    };
    let (prefix_args, observation_requested) = match observation_arguments(prefix_args) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("cu-host: {error}");
            return ExitCode::from(2);
        }
    };
    let mut stdin = std::io::stdin();
    let ((owned_bootstrap, model_bootstrap), revisit_requested) =
        match read_startup_bootstraps(prefix_args, &mut stdin) {
            Ok(bootstraps) => bootstraps,
            Err(error) => {
                eprintln!("cu-host: {error}");
                return ExitCode::from(2);
            }
        };
    if let Err(error) =
        validate_observation_authorization(observation_requested, model_bootstrap.as_ref())
    {
        eprintln!("cu-host: {error}");
        return ExitCode::from(2);
    }
    // Failure of optional observation cannot invalidate the authorized teacher.
    let teacher_observation = if observation_requested {
        open_inherited_fd4()
            .and_then(TeacherObservationWriter::new)
            .ok()
    } else {
        None
    };
    let mut input = FramedReader::new(stdin);
    let configure = match input.read_next() {
        Ok(Some(envelope)) => match envelope.message {
            Message::Configure(configure) if configure.role == PeerRole::Host => configure,
            other => {
                eprintln!(
                    "cu-host: expected Configure for the host, got {}",
                    other.name()
                );
                return ExitCode::from(2);
            }
        },
        Ok(None) | Err(_) => {
            eprintln!("cu-host: the supervisor did not configure it");
            return ExitCode::from(2);
        }
    };
    let config = match HostConfig::from_configure_with_owned_bootstrap(&configure, owned_bootstrap)
    {
        Ok(config) => config,
        Err(error) => {
            eprintln!("cu-host: {error}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = validate_owned_save_model_authorization(&config, model_bootstrap.as_ref()) {
        eprintln!("cu-host: {error}");
        return ExitCode::from(2);
    }
    if let Err(error) = validate_computer_image_authorization(
        computer_image_requested,
        model_bootstrap.as_ref(),
        &config,
    ) {
        eprintln!("cu-host: {error}");
        return ExitCode::from(2);
    }
    let computer_image = if computer_image_requested {
        match desktop_host::computer_image::open_inherited_fd5() {
            Ok(stream) => Some(stream),
            Err(error) => {
                eprintln!("cu-host: {error}");
                return ExitCode::from(2);
            }
        }
    } else {
        None
    };
    let owned_revisit_control = if revisit_requested {
        if let Err(error) = validate_revisit_control_authorization(revisit_requested, &config) {
            eprintln!("cu-host: {error}");
            return ExitCode::from(2);
        }
        match desktop_host::open_owned_revisit_control_fd3() {
            Ok(stream) => Some(stream),
            Err(error) => {
                eprintln!("cu-host: {error}");
                return ExitCode::from(2);
            }
        }
    } else {
        if let Err(error) = validate_revisit_control_authorization(false, &config) {
            eprintln!("cu-host: {error}");
            return ExitCode::from(2);
        }
        None
    };
    let link = Link::new(std::io::stdout(), "host", config.clock);
    let services = match (config.planning_backend, model_bootstrap) {
        (desktop_host::config::PlanningBackend::Model, Some(bootstrap))
            if bootstrap.authorizes(ModelPurpose::S2Planning) =>
        {
            bootstrap
        }
        (desktop_host::config::PlanningBackend::Model, _) => {
            eprintln!(
                "cu-host: model backend requires matching private authorization with s2_planning purpose"
            );
            return ExitCode::from(2);
        }
        (_, Some(bootstrap)) if bootstrap.authorizes(ModelPurpose::S2Planning) => {
            eprintln!("cu-host: s2_planning authorization requires the model backend");
            return ExitCode::from(2);
        }
        (_, Some(bootstrap)) => bootstrap,
        (_, None) => {
            return match shell::run_with_models(
                config,
                link,
                input,
                HostModelServices::default(),
                owned_revisit_control,
                teacher_observation,
                computer_image,
            ) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("cu-host: {error}");
                    ExitCode::FAILURE
                }
            };
        }
    };
    let services = match services.into_services(config.clock) {
        Ok(services) => services,
        Err(error) => {
            eprintln!("cu-host: {error}");
            return ExitCode::from(2);
        }
    };
    match shell::run_with_models(
        config,
        link,
        input,
        services,
        owned_revisit_control,
        teacher_observation,
        computer_image,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cu-host: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn frame(value: serde_json::Value) -> Vec<u8> {
        let payload = serde_json::to_vec(&value).unwrap();
        let mut bytes = (payload.len() as u32).to_be_bytes().to_vec();
        bytes.extend(payload);
        bytes
    }

    fn owned_payload() -> serde_json::Value {
        json!({
            "version": 1,
            "registration_id": "123e4567-e89b-42d3-a456-426614174000",
            "pid": 1234,
            "start_seconds": 1_700_000_000,
            "start_micros": 123456,
            "executable_path": "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit",
            "bundle_id": "com.apple.TextEdit",
            "document_path": "/private/tmp/owned/document.txt",
            "document_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "document_dev": 1,
            "document_ino": 2,
            "directory_dev": 3,
            "directory_ino": 4,
            "issued_unix_ms": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64,
        })
    }

    fn model_payload() -> serde_json::Value {
        json!({
            "purposes": ["s1_teacher"],
            "credential": { "kind": "api_key", "api_key": "test-only-private-key" },
            "route": {
                "provider_id": "open_ai",
                "protocol": "open_ai_chat",
                "base_url": "https://example.invalid/v1",
                "model": "test-model",
                "max_output_tokens": 4096
            }
        })
    }

    fn model_bootstrap_for(purposes: &[&str]) -> ModelBootstrap {
        let mut payload = model_payload();
        payload["purposes"] = json!(purposes);
        let input = frame(payload);
        ModelBootstrap::read(&mut &input[..]).unwrap()
    }

    #[test]
    fn computer_image_flag_is_last_after_the_teacher_flag_and_requires_bootstrap() {
        let args = [
            "--model-bootstrap",
            "--teacher-observation",
            "--computer-image",
        ]
        .map(String::from);
        let (prefix, computer) = computer_image_arguments(&args).unwrap();
        let (prefix, teacher) = observation_arguments(prefix).unwrap();
        assert_eq!(prefix, ["--model-bootstrap"]);
        assert!(computer && teacher);
        for args in [
            vec!["--computer-image"],
            vec!["--computer-image", "--model-bootstrap"],
            vec![
                "--model-bootstrap",
                "--computer-image",
                "--teacher-observation",
            ],
            vec!["--model-bootstrap", "--computer-image", "--computer-image"],
        ] {
            let args = args.into_iter().map(String::from).collect::<Vec<_>>();
            assert!(computer_image_arguments(&args).is_err());
        }
    }

    #[test]
    fn computer_image_needs_its_own_purpose_and_an_explicit_native_window() {
        let mut config = owned_model_config();
        let mut payload = model_payload();
        payload["purposes"] = json!(["computer_proposals"]);
        payload["account_scope"] = json!("123e4567-e89b-42d3-a456-426614174003");
        payload["route"]["base_url"] = json!("https://api.openai.com/v1");
        payload["route"]["protocol"] = json!("open_ai_responses");
        let bytes = frame(payload);
        let bootstrap = ModelBootstrap::read(&mut &bytes[..]).unwrap();
        assert!(validate_computer_image_authorization(true, Some(&bootstrap), &config).is_err());
        config.native_window_scope = Some(interaction_engine::engine::NativeWindowScope {
            app: "com.apple.TextEdit".into(),
            pid: 1234,
            window: "window-computer-test".into(),
        });
        assert!(validate_computer_image_authorization(true, Some(&bootstrap), &config).is_ok());
        assert!(validate_computer_image_authorization(false, Some(&bootstrap), &config).is_err());
        assert!(
            validate_computer_image_authorization(
                true,
                Some(&model_bootstrap_for(&["s1_teacher"])),
                &config
            )
            .is_err()
        );
        assert!(validate_computer_image_authorization(true, None, &config).is_err());
        assert!(validate_computer_image_authorization(false, None, &config).is_ok());
    }

    #[test]
    fn private_teacher_observation_flag_is_last_and_requires_separate_s1_purpose() {
        for prefix in [
            vec!["--model-bootstrap"],
            vec![
                "--owned-document-bootstrap",
                "--model-bootstrap",
                "--owned-revisit-control",
            ],
        ] {
            let mut args = prefix.into_iter().map(String::from).collect::<Vec<_>>();
            let expected = args.clone();
            args.push("--teacher-observation".into());
            let (actual, requested) = observation_arguments(&args).unwrap();
            assert_eq!(actual, expected);
            assert!(requested);
        }
        for args in [
            vec!["--teacher-observation"],
            vec!["--teacher-observation", "--model-bootstrap"],
            vec![
                "--model-bootstrap",
                "--teacher-observation",
                "--teacher-observation",
            ],
        ] {
            let args = args.into_iter().map(String::from).collect::<Vec<_>>();
            assert!(observation_arguments(&args).is_err());
        }
        assert!(validate_observation_authorization(true, None).is_err());
        assert!(
            validate_observation_authorization(true, Some(&model_bootstrap_for(&["s2_planning"])))
                .is_err()
        );
        assert!(
            validate_observation_authorization(true, Some(&model_bootstrap_for(&["s1_teacher"])))
                .is_ok()
        );
        assert!(validate_observation_authorization(false, None).is_ok());
    }

    fn owned_bootstrap() -> OwnedDocumentBootstrap {
        let input = frame(owned_payload());
        OwnedDocumentBootstrap::read(&mut &input[..]).unwrap()
    }

    fn owned_model_config() -> HostConfig {
        let configure = interaction_contracts::desktop::handshake::Configure {
            role: PeerRole::Host,
            session_epoch: 1,
            settings: [
                ("socket_dir", "/tmp/host"),
                ("db_path", "/tmp/host/host.db"),
                ("device_token", "0123456789abcdef0123456789abcdef"),
                ("device_epoch", "1"),
                ("source_token", "fedcba9876543210fedcba9876543210"),
                ("clock_origin_ms", "1800000000000"),
                ("planning_backend", "model"),
                ("owned_save", "1"),
                ("native_target_bundle", "com.apple.TextEdit"),
                ("native_target_pid", "1234"),
            ]
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .into(),
            browser_page_profile: None,
        };
        HostConfig::from_configure_with_owned_bootstrap(&configure, Some(owned_bootstrap()))
            .unwrap()
    }

    #[test]
    fn the_cli_accepts_only_the_documented_prefix_orders() {
        let mut bytes = frame(owned_payload());
        bytes.extend(frame(model_payload()));
        bytes.extend_from_slice(b"CONFIGURE");
        let mut input = &bytes[..];
        let args = vec![
            "--owned-document-bootstrap".to_string(),
            "--model-bootstrap".to_string(),
        ];
        let ((owned, model), revisit) = read_startup_bootstraps(&args, &mut input).unwrap();
        assert!(owned.is_some());
        assert!(model.unwrap().authorizes(ModelPurpose::S1Teacher));
        assert!(!revisit);
        assert_eq!(input, b"CONFIGURE");

        let mut model_only = frame(model_payload());
        model_only.extend_from_slice(b"CONFIGURE");
        let mut input = &model_only[..];
        let args = vec!["--model-bootstrap".to_string()];
        let ((owned, model), revisit) = read_startup_bootstraps(&args, &mut input).unwrap();
        assert!(owned.is_none());
        assert!(model.unwrap().authorizes(ModelPurpose::S1Teacher));
        assert!(!revisit);
        assert_eq!(input, b"CONFIGURE");
    }

    #[test]
    fn revisit_control_flag_requires_owned_bootstrap_and_follows_existing_prefixes() {
        let mut bytes = frame(owned_payload());
        bytes.extend(frame(model_payload()));
        bytes.extend_from_slice(b"CONFIGURE");
        let mut input = &bytes[..];
        let args = vec![
            "--owned-document-bootstrap".to_string(),
            "--model-bootstrap".to_string(),
            "--owned-revisit-control".to_string(),
        ];
        let ((owned, model), revisit) = read_startup_bootstraps(&args, &mut input).unwrap();
        assert!(owned.is_some());
        assert!(model.unwrap().authorizes(ModelPurpose::S1Teacher));
        assert!(revisit);
        assert_eq!(input, b"CONFIGURE");

        for args in [
            vec!["--owned-revisit-control"],
            vec!["--model-bootstrap", "--owned-revisit-control"],
            vec!["--owned-document-bootstrap", "--model-bootstrap", "--other"],
        ] {
            let args = args.into_iter().map(String::from).collect::<Vec<_>>();
            let mut input = b"private-prefix".as_slice();
            assert!(read_startup_bootstraps(&args, &mut input).is_err());
            assert_eq!(input, b"private-prefix");
        }

        let mut bytes = frame(owned_payload());
        bytes.extend_from_slice(b"CONFIGURE");
        let mut input = &bytes[..];
        let args = vec![
            "--owned-document-bootstrap".to_string(),
            "--owned-revisit-control".to_string(),
        ];
        let ((owned, model), revisit) = read_startup_bootstraps(&args, &mut input).unwrap();
        assert!(owned.is_some());
        assert!(model.is_none());
        assert!(revisit);
        assert_eq!(input, b"CONFIGURE");
    }

    #[test]
    fn revisit_flag_does_not_authorize_itself_without_save_and_registration() {
        let configure = interaction_contracts::desktop::handshake::Configure {
            role: PeerRole::Host,
            session_epoch: 1,
            settings: [
                ("socket_dir", "/tmp/host"),
                ("db_path", "/tmp/host/host.db"),
                ("device_token", "0123456789abcdef0123456789abcdef"),
                ("device_epoch", "1"),
                ("source_token", "fedcba9876543210fedcba9876543210"),
                ("clock_origin_ms", "1800000000000"),
            ]
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .into(),
            browser_page_profile: None,
        };
        let config = HostConfig::from_configure(&configure).unwrap();
        assert!(validate_revisit_control_authorization(false, &config).is_ok());
        assert!(validate_revisit_control_authorization(true, &config).is_err());
    }

    #[test]
    fn owned_save_model_requires_a_private_s2_prefix_and_enables_revisit_only_with_it() {
        let config = owned_model_config();
        assert!(validate_owned_save_model_authorization(&config, None).is_err());
        assert!(
            validate_owned_save_model_authorization(
                &config,
                Some(&model_bootstrap_for(&["s1_teacher"])),
            )
            .is_err()
        );

        let s2 = model_bootstrap_for(&["s2_planning"]);
        assert!(validate_owned_save_model_authorization(&config, Some(&s2)).is_ok());
        assert!(validate_revisit_control_authorization(true, &config).is_ok());
    }

    #[test]
    fn reversed_duplicate_and_unknown_arguments_are_rejected_before_reading() {
        for args in [
            vec!["--model-bootstrap", "--owned-document-bootstrap"],
            vec![
                "--owned-document-bootstrap",
                "--model-bootstrap",
                "--model-bootstrap",
            ],
            vec!["--model-bootstrap", "--model-bootstrap"],
            vec!["--something-else"],
        ] {
            let args = args.into_iter().map(String::from).collect::<Vec<_>>();
            let mut input = b"private-bytes".as_slice();
            let error = read_startup_bootstraps(&args, &mut input).err().unwrap();
            assert_eq!(error, "unsupported startup arguments");
            assert_eq!(input, b"private-bytes");
        }
    }
}
