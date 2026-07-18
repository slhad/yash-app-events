use std::fs;
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use clap::{Parser, Subcommand};
use yash_app_events_catalog::{
    build_package, catalog_filename, Catalog, CatalogEntry, CATALOG_SCHEMA_VERSION,
};

#[derive(Debug, Parser)]
#[command(about = "Validate and build the public profile catalog")]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Validate {
        #[arg(default_value = "catalog/profiles")]
        root: PathBuf,
    },
    Build {
        #[arg(default_value = "catalog/profiles")]
        root: PathBuf,
        #[arg(long, default_value = "target/profile-catalog")]
        output: PathBuf,
    },
    Index {
        entries: PathBuf,
        #[arg(long)]
        revision: u64,
        #[arg(long, default_value = "target/profile-catalog")]
        output: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse();
    match arguments.command {
        Command::Validate { root } => {
            let directories = source_directories(&root)?;
            for directory in &directories {
                yash_app_events_catalog::validate_source_directory(directory)?;
                println!("validated {}", directory.display());
            }
            if directories.is_empty() {
                anyhow::bail!("catalog contains no profile sources");
            }
        }
        Command::Build { root, output } => {
            fs::create_dir_all(&output)?;
            let mut entries = source_directories(&root)?
                .iter()
                .map(|directory| build_package(directory, &output))
                .collect::<Result<Vec<_>, _>>()?;
            entries.sort_by(|left, right| {
                left.id
                    .cmp(&right.id)
                    .then_with(|| left.version.cmp(&right.version))
            });
            ensure_unique_entries(&entries)?;
            let destination = output.join("catalog-entries.json");
            fs::write(&destination, serde_json::to_vec_pretty(&entries)?)?;
            println!("built {} profile package(s)", entries.len());
            println!("entries {}", destination.display());
        }
        Command::Index {
            entries,
            revision,
            output,
        } => {
            let profiles: Vec<CatalogEntry> = serde_json::from_slice(&fs::read(entries)?)?;
            let catalog = Catalog {
                schema: CATALOG_SCHEMA_VERSION,
                revision,
                generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                profiles,
            };
            catalog.validate()?;
            fs::create_dir_all(&output)?;
            let destination = output.join(catalog_filename(revision));
            fs::write(&destination, serde_json::to_vec_pretty(&catalog)?)?;
            println!("index {}", destination.display());
        }
    }
    Ok(())
}

fn source_directories(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    visit(root, &mut directories)?;
    directories.sort();
    Ok(directories)
}

fn visit(current: &Path, directories: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            anyhow::bail!(
                "catalog source contains symlink: {}",
                entry.path().display()
            );
        }
        if file_type.is_dir() {
            visit(&entry.path(), directories)?;
        } else if entry.file_name() == "catalog-entry.json" {
            let parent = entry
                .path()
                .parent()
                .ok_or_else(|| anyhow::anyhow!("catalog entry has no parent"))?
                .to_path_buf();
            directories.push(parent);
        }
    }
    Ok(())
}

fn ensure_unique_entries(entries: &[CatalogEntry]) -> anyhow::Result<()> {
    for (index, entry) in entries.iter().enumerate() {
        if entries[..index].iter().any(|previous| {
            previous.id == entry.id && previous.version == entry.version
                || previous.package == entry.package
                || previous.profile_id == entry.profile_id
        }) {
            anyhow::bail!("duplicate catalog entry: {} {}", entry.id, entry.version);
        }
    }
    Ok(())
}
