use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use rbx_inject::{apply, check::Severity, config::Config, inputs, inputs::Inputs};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "rbx-inject",
    version,
    about = "Inject asset ids and config values into a Roblox place file",
    long_about = "Reads a .rbxl, applies injection rules against an asphalt asset map, \
                  and writes the place back out. No network and no credentials: the \
                  upload is somebody else's job."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Apply injection rules to a place file.
    Apply(Apply),
    /// Validate rules against a place without changing anything. Needs no asset
    /// map and no credentials, so it runs in a pre-commit hook or in CI.
    Check(Check),
    /// Rewrite a JSON injections file as TOML, leaving the original in place.
    Migrate(Migrate),
}

#[derive(Parser)]
struct Apply {
    /// The .rbxl to inject into.
    #[arg(long, value_name = "PATH")]
    place: PathBuf,

    /// Injection rules, .toml or .json.
    #[arg(long, value_name = "PATH")]
    config: PathBuf,

    /// Asphalt output (Assets.luau). Read as data for the asset map, and
    /// registered as the module named `assets`, which is what `$module` means.
    #[arg(long, value_name = "PATH")]
    assets: Option<PathBuf>,

    /// Register a generated Luau file as an injectable module source:
    /// `--module ids=src/shared/GameIds.luau`, injected with `$module:ids`.
    /// Repeatable.
    #[arg(long, value_name = "NAME=PATH")]
    module: Vec<String>,

    /// Shorthand for `--module ids=<PATH>`, which is what `$rbxsync_module`
    /// means.
    #[arg(long, value_name = "PATH", alias = "rbxsync-module")]
    ids_module: Option<PathBuf>,

    /// Where to write. Defaults to editing --place in place.
    #[arg(long, short, value_name = "PATH")]
    output: Option<PathBuf>,

    /// Report what would change and write nothing.
    #[arg(long)]
    dry_run: bool,

    /// Treat unresolved rules as errors. Use it in CI.
    #[arg(long)]
    strict: bool,
}

#[derive(Parser)]
struct Check {
    /// The .rbxl to validate against.
    #[arg(long, value_name = "PATH")]
    place: PathBuf,

    /// Injection rules, .toml or .json.
    #[arg(long, value_name = "PATH")]
    config: PathBuf,

    /// Asphalt output. Optional: without it, asset-map lookups are not checked,
    /// which is what lets this run before anything has been uploaded.
    #[arg(long, value_name = "PATH")]
    assets: Option<PathBuf>,

    /// Fail on warnings too, not only on errors.
    #[arg(long)]
    strict: bool,
}

#[derive(Parser)]
struct Migrate {
    /// The .json injections file to convert.
    #[arg(value_name = "PATH")]
    config: PathBuf,

    /// Where to write the TOML. Defaults to the input with a .toml extension.
    #[arg(long, short, value_name = "PATH")]
    output: Option<PathBuf>,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Apply(args) => run_apply(args),
        Command::Check(args) => run_check(args),
        Command::Migrate(args) => run_migrate(args),
    }
}

fn read_place(path: &Path) -> Result<rbx_dom_weak::WeakDom> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    rbx_binary::from_reader(bytes.as_slice()).with_context(|| format!("parsing {}", path.display()))
}

fn run_check(args: Check) -> Result<()> {
    let config = Config::load(&args.config)?;
    let dom = read_place(&args.place)?;

    // Deliberately not `require_inputs`: running with no asset map at all is the
    // point of `check`, so that a pre-commit hook can validate paths long before
    // anything has been uploaded.
    let inputs = match &args.assets {
        Some(path) => Some(Inputs::default().with_asphalt(path)?),
        None => None,
    };

    let findings = rbx_inject::check::check(&dom, &config, inputs.as_ref());

    let rules = config.active().count();
    let errors = findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .count();
    let warnings = findings.len() - errors;

    for finding in &findings {
        eprintln!("  {finding}");
    }

    if findings.is_empty() {
        println!("{rules} rule(s), all resolve.");
        return Ok(());
    }

    println!("{rules} rule(s): {errors} error(s), {warnings} warning(s).");

    if errors > 0 || (args.strict && warnings > 0) {
        std::process::exit(1);
    }

    Ok(())
}

/// `name=path`, the shape of a `--module` argument.
fn parse_module_arg(arg: &str) -> Result<(String, PathBuf)> {
    let (name, path) = arg
        .split_once('=')
        .with_context(|| format!("--module wants NAME=PATH, got '{arg}'"))?;

    if name.is_empty() {
        bail!("--module wants a name before the '=', got '{arg}'");
    }

    Ok((name.to_string(), PathBuf::from(path)))
}

fn build_inputs(
    assets: Option<&Path>,
    ids_module: Option<&Path>,
    modules: &[String],
) -> Result<Inputs> {
    let mut inputs = Inputs::default();

    if let Some(path) = assets {
        inputs = inputs.with_asphalt(path)?;
    }
    if let Some(path) = ids_module {
        inputs = inputs.with_module(inputs::IDS, path)?;
    }
    for arg in modules {
        let (name, path) = parse_module_arg(arg)?;
        inputs = inputs.with_module(&name, &path)?;
    }

    Ok(inputs)
}

/// Refuse before touching the place when an input the config needs is missing.
///
/// Without this, a forgotten `--assets` resolves nothing and reads exactly like
/// a place that has drifted away from its rules: a wall of warnings, an exit
/// code of zero, and a deploy that carries on.
fn require_inputs(config: &Config, inputs: &Inputs) -> Result<()> {
    let needs = config.needs();

    if needs.asset_map && !inputs.has_map() {
        bail!("this config looks keys up in the asset map; pass --assets <asphalt's Assets.luau>");
    }

    for name in &needs.modules {
        if inputs.module(name).is_none() {
            let hint = match name.as_str() {
                inputs::ASSETS => "--assets <path>".to_string(),
                other => format!("--module {other}=<path>"),
            };
            let known: Vec<&str> = inputs.module_names().collect();
            bail!(
                "this config injects the module '{name}', which was not given; pass {hint}{}",
                if known.is_empty() {
                    String::new()
                } else {
                    format!(" (given: {})", known.join(", "))
                }
            );
        }
    }

    Ok(())
}

fn run_apply(args: Apply) -> Result<()> {
    let config = Config::load(&args.config)?;
    let inputs = build_inputs(
        args.assets.as_deref(),
        args.ids_module.as_deref(),
        &args.module,
    )?;
    require_inputs(&config, &inputs)?;

    let mut dom = read_place(&args.place)?;
    let report = apply(&mut dom, &config, &inputs);

    for line in &report.changes {
        println!("  {line}");
    }
    for line in &report.warnings {
        eprintln!("  warning: {line}");
    }

    if !report.changed() {
        println!("Nothing to inject.");
    }

    if args.strict && !report.warnings.is_empty() {
        bail!(
            "{} rule(s) did not resolve, and --strict was given",
            report.warnings.len()
        );
    }

    if args.dry_run {
        println!("Dry run, nothing written.");
        return Ok(());
    }

    if !report.changed() {
        return Ok(());
    }

    let output = args.output.as_deref().unwrap_or(&args.place);
    write_place(&dom, output)?;
    println!(
        "Wrote {} ({} change(s)).",
        output.display(),
        report.changes.len()
    );

    Ok(())
}

fn write_place(dom: &rbx_dom_weak::WeakDom, output: &Path) -> Result<()> {
    let children = dom
        .get_by_ref(dom.root_ref())
        .context("place has no root")?
        .children()
        .to_vec();

    let mut bytes = Vec::new();
    rbx_binary::to_writer(&mut bytes, dom, &children).context("serializing the place")?;

    // Write beside the target and rename, so an interrupted run cannot leave a
    // truncated .rbxl where a working one used to be. The place file is often
    // the only copy of a build that took a while to make.
    let temp = output.with_extension("rbxl.tmp");
    std::fs::write(&temp, &bytes).with_context(|| format!("writing {}", temp.display()))?;
    std::fs::rename(&temp, output).with_context(|| format!("replacing {}", output.display()))?;

    Ok(())
}

fn run_migrate(args: Migrate) -> Result<()> {
    let config = Config::load(&args.config)?;
    let toml = config.to_toml()?;

    let output = args
        .output
        .unwrap_or_else(|| args.config.with_extension("toml"));

    if output.exists() {
        bail!("{} already exists", output.display());
    }

    std::fs::write(&output, &toml).with_context(|| format!("writing {}", output.display()))?;

    println!(
        "Wrote {} ({} rule(s)). The original is untouched.",
        output.display(),
        config.injections.len()
    );

    Ok(())
}
