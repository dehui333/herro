use clap::{Args, Parser, Subcommand};

use ont_haec_rs::{error_correction, generate_features};

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(global = true)]
    reads: String,

    #[arg(global = true)]
    overlaps: String,

    #[arg(short = 'w', default_value = "4096", global = true)]
    window_size: u32,

    #[arg(short = 't', default_value = "1", global = true)]
    feat_gen_threads: usize,

    #[arg(global = true)]
    output: String,
}

#[derive(Subcommand)]
enum Commands {
    Features,
    Inference(InferenceArgs),
}

#[derive(Args)]
struct InferenceArgs {
    #[arg(short = 'm')]
    model: String,

    #[arg(short = 'd', value_delimiter = ',', default_value = "0")]
    devices: Vec<usize>,
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Features => {
            generate_features(
                &cli.reads,
                &cli.overlaps,
                &cli.output,
                cli.feat_gen_threads,
                cli.window_size,
            );
        }
        Commands::Inference(args) => error_correction(
            &cli.reads,
            &cli.overlaps,
            &args.model,
            &cli.output,
            cli.feat_gen_threads,
            cli.window_size,
            &args.devices,
        ),
    }
}
