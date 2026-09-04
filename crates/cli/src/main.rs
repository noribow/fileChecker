//! File Checker CLI entry point (`docs/requirements.md` §10.13/§10.16). A thin layer
//! over `filechecker-core`: argument parsing, delegation, and result formatting only —
//! no check/scan logic lives here (§10.13's "CLI holds no core-specific logic").

mod commands;
mod db;
mod exit;
mod output;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use output::Format;

#[derive(Parser)]
#[command(
    name = "filechecker",
    about = "File Checker CLI (GUIの機能の部分集合。詳細はrequirements.md §10.13/§10.16を参照)"
)]
struct Cli {
    /// SQLite結果DBのパス（存在しなければ新規作成）
    #[arg(long)]
    db: PathBuf,

    /// 進捗・状態表示(stderr)を抑止する
    #[arg(long)]
    quiet: bool,

    #[command(subcommand)]
    command: TopCommand,
}

#[derive(Subcommand)]
enum TopCommand {
    /// スキャン（情報取得フェーズ, §10.3）
    #[command(subcommand)]
    Scan(ScanCommand),
    /// リムーバブルメディア（§10.4/§6）
    #[command(subcommand)]
    Media(MediaCommand),
    /// お手本セット（§3.4）
    #[command(subcommand)]
    Reference(ReferenceCommand),
    /// 比較（比較フェーズ, §10.3）
    #[command(subcommand)]
    Check(CheckCommand),
    /// レポート出力（§7）
    #[command(subcommand)]
    Report(ReportCommand),
    /// 設定（§10.6/§10.7）
    #[command(subcommand)]
    Config(ConfigCommand),
}

#[derive(Subcommand)]
enum ScanCommand {
    /// フォルダをスキャンしscan_runを作成
    Folder {
        path: PathBuf,
        /// (現時点では常に新規スキャンを行うため無効: 過去scan_runの再利用は未実装)
        #[arg(long)]
        rescan: bool,
    },
    /// 接続中のリムーバブルメディアをスキャン（§10.8のeager方式）
    Media {
        #[arg(long = "media-id")]
        media_id: Option<i64>,
        #[arg(long)]
        mount: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum MediaCommand {
    /// 既知メディア一覧（表示名・識別子種別・最終確認日時）
    List,
}

#[derive(Subcommand)]
enum ReferenceCommand {
    /// 既存scan_runからお手本セットを生成（§10.12の経年変化検知の起点）
    Generate {
        #[arg(long = "from-scan")]
        from_scan: i64,
        #[arg(long)]
        name: String,
        #[arg(long)]
        supersede: Option<i64>,
    },
    /// お手本セット一覧（supersedesのバージョン履歴含む）
    List,
}

#[derive(Subcommand)]
enum CheckCommand {
    /// 整合性チェックを実行
    Integrity {
        #[arg(long = "reference-set")]
        reference_set: i64,
        #[arg(long)]
        folder: Option<PathBuf>,
        #[arg(long = "scan-run")]
        scan_run: Vec<i64>,
        #[arg(long, default_value = "text")]
        format: Format,
        #[arg(long)]
        output: Option<PathBuf>,
        /// 明細を絞り込むステータス（カンマ区切り、既定はok以外全て）
        #[arg(long, value_delimiter = ',')]
        status: Vec<String>,
        /// 差分のみ(コード1)を0として扱う
        #[arg(long = "exit-zero-on-diff")]
        exit_zero_on_diff: bool,
    },
    /// 重複チェックを実行（複数指定可、保存済みscan_runを混在させられる）
    Duplicate {
        #[arg(long)]
        folder: Vec<PathBuf>,
        #[arg(long = "scan-run")]
        scan_run: Vec<i64>,
        #[arg(long, default_value = "text")]
        format: Format,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long = "exit-zero-on-diff")]
        exit_zero_on_diff: bool,
    },
    /// 過去のcheck_run一覧
    List {
        #[arg(long = "type")]
        r#type: Option<String>,
        #[arg(long)]
        limit: Option<i64>,
    },
    /// 過去のcheck_run結果を再表示
    Show {
        check_run_id: i64,
        #[arg(long, default_value = "text")]
        format: Format,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long, value_delimiter = ',')]
        status: Vec<String>,
    },
}

#[derive(Subcommand)]
enum ReportCommand {
    /// check_runの結果をファイルへ出力（csv|json。html/textはP13で追加予定）
    Export {
        check_run_id: i64,
        #[arg(long)]
        format: Format,
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// app_settingの参照
    Get { key: Option<String> },
    /// app_settingの変更
    Set { key: String, value: String },
}

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            let code = match e.kind() {
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
                    exit::SUCCESS
                }
                _ => exit::USAGE_ERROR,
            };
            std::process::exit(code);
        }
    };

    let mut conn = match db::open_db(&cli.db) {
        Ok(conn) => conn,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(exit::FAILURE);
        }
    };

    let result = match cli.command {
        TopCommand::Scan(ScanCommand::Folder { path, rescan: _ }) => {
            commands::scan_folder(&mut conn, &path, cli.quiet)
        }
        TopCommand::Scan(ScanCommand::Media { media_id, mount }) => {
            commands::scan_media(&mut conn, media_id, mount, cli.quiet)
        }
        TopCommand::Media(MediaCommand::List) => commands::media_list(&conn),
        TopCommand::Reference(ReferenceCommand::Generate {
            from_scan,
            name,
            supersede,
        }) => commands::reference_generate(&mut conn, from_scan, &name, supersede),
        TopCommand::Reference(ReferenceCommand::List) => commands::reference_list(&conn),
        TopCommand::Check(CheckCommand::Integrity {
            reference_set,
            folder,
            scan_run,
            format,
            output,
            status,
            exit_zero_on_diff,
        }) => commands::check_integrity(
            &mut conn,
            reference_set,
            folder,
            scan_run,
            format,
            output,
            status,
            exit_zero_on_diff,
            cli.quiet,
        ),
        TopCommand::Check(CheckCommand::Duplicate {
            folder,
            scan_run,
            format,
            output,
            exit_zero_on_diff,
        }) => commands::check_duplicate(
            &mut conn,
            folder,
            scan_run,
            format,
            output,
            exit_zero_on_diff,
            cli.quiet,
        ),
        TopCommand::Check(CheckCommand::List { r#type, limit }) => {
            commands::check_list(&conn, r#type, limit)
        }
        TopCommand::Check(CheckCommand::Show {
            check_run_id,
            format,
            output,
            status,
        }) => commands::check_show(&conn, check_run_id, format, output, status),
        TopCommand::Report(ReportCommand::Export {
            check_run_id,
            format,
            output,
        }) => commands::report_export(&conn, check_run_id, format, output),
        TopCommand::Config(ConfigCommand::Get { key }) => commands::config_get(&conn, key),
        TopCommand::Config(ConfigCommand::Set { key, value }) => {
            commands::config_set(&conn, &key, &value)
        }
    };

    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {}", e.message);
            std::process::exit(e.exit_code);
        }
    }
}
