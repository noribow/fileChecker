//! File Checker CLI entry point (`docs/requirements.md` §10.13/§10.16). A thin layer
//! over `filechecker-core`: argument parsing, delegation, and result formatting only —
//! no check/scan logic lives here (§10.13's "CLI holds no core-specific logic").

mod commands;
mod db;
mod exit;
mod output;
mod password_policy;

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

    /// 登録パスワード設定ファイルのパス（§10.9/§10.10）。app_settingの
    /// archive_password_mode が try_registered の場合、パスワード保護アーカイブの
    /// 復号にこのファイル内の登録パスワードを試みる（マスターパスワードの入力が必要）。
    #[arg(long = "password-store")]
    password_store: Option<PathBuf>,

    /// app_settingのarchive_password_modeに関わらず、パスワード保護アーカイブを
    /// 常にエラー扱いにする（§10.7モード1）。
    #[arg(long = "no-archive-password")]
    no_archive_password: bool,

    /// エラーログファイル（§10.17/§10.22）の出力先ディレクトリ。指定した場合のみ
    /// scan_run/check_run単位のテキストログを書き出す（ローテーション・自動削除
    /// なし、永続保持）。未指定なら書き出さない。
    #[arg(long = "log-dir")]
    log_dir: Option<PathBuf>,

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
    /// 再構成（§10.19/§10.20）
    #[command(subcommand)]
    Reconstruct(ReconstructCommand),
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
    /// 外部お手本セット定義ファイルを取り込む（§10.18、現状MAME形式のみ）
    Import {
        #[arg(long)]
        file: PathBuf,
        /// mame-softwarelist | mame-machinelist
        #[arg(long)]
        format: String,
        #[arg(long)]
        name: String,
        /// mame-machinelistでは必須（merged|split）。§10.18の通り自動判定は行わない
        #[arg(long = "merge-mode")]
        merge_mode: Option<String>,
        /// status=baddumpのエントリも取り込む（既定は除外）
        #[arg(long = "include-baddump")]
        include_baddump: bool,
    },
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
    /// check_runの結果をファイルへ出力（csv|json|html。textはcheck showを使用）
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

#[derive(Subcommand)]
enum ReconstructCommand {
    /// 充当計画を算出（実行はしない）
    Plan {
        #[arg(long = "check-run")]
        check_run: i64,
        #[arg(long)]
        destination: PathBuf,
    },
    /// 再構成を実行（既存のreconstruction_run再開、または--check-run+--destinationで新規）
    Run {
        reconstruction_run_id: Option<i64>,
        #[arg(long = "check-run")]
        check_run: Option<i64>,
        #[arg(long)]
        destination: Option<PathBuf>,
    },
    /// 実行状況・完了報告の再表示
    Status { reconstruction_run_id: i64 },
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

    let resolved_policy = match password_policy::resolve(
        &conn,
        cli.no_archive_password,
        cli.password_store.as_deref(),
    ) {
        Ok(resolved) => resolved,
        Err((message, code)) => {
            eprintln!("error: {message}");
            std::process::exit(code);
        }
    };
    let policy = resolved_policy.as_policy();

    let log_dir = cli.log_dir.as_deref();

    let result = match cli.command {
        TopCommand::Scan(ScanCommand::Folder { path, rescan: _ }) => {
            commands::scan_folder(&mut conn, &path, cli.quiet, &policy, log_dir)
        }
        TopCommand::Scan(ScanCommand::Media { media_id, mount }) => {
            commands::scan_media(&mut conn, media_id, mount, cli.quiet, &policy, log_dir)
        }
        TopCommand::Media(MediaCommand::List) => commands::media_list(&conn),
        TopCommand::Reference(ReferenceCommand::Generate {
            from_scan,
            name,
            supersede,
        }) => {
            commands::reference_generate(&mut conn, from_scan, &name, supersede, &policy, log_dir)
        }
        TopCommand::Reference(ReferenceCommand::List) => commands::reference_list(&conn),
        TopCommand::Reference(ReferenceCommand::Import {
            file,
            format,
            name,
            merge_mode,
            include_baddump,
        }) => commands::reference_import(
            &mut conn,
            &file,
            &format,
            &name,
            merge_mode.as_deref(),
            include_baddump,
        ),
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
            &policy,
            log_dir,
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
            &policy,
            log_dir,
        ),
        TopCommand::Check(CheckCommand::List { r#type, limit }) => {
            commands::check_list(&conn, r#type, limit)
        }
        TopCommand::Check(CheckCommand::Show {
            check_run_id,
            format,
            output,
            status,
        }) => commands::check_show_cli(&conn, check_run_id, format, output, status),
        TopCommand::Report(ReportCommand::Export {
            check_run_id,
            format,
            output,
        }) => commands::report_export(&conn, check_run_id, format, output),
        TopCommand::Reconstruct(ReconstructCommand::Plan {
            check_run,
            destination,
        }) => commands::reconstruct_plan(&mut conn, check_run, &destination, cli.quiet, &policy),
        TopCommand::Reconstruct(ReconstructCommand::Run {
            reconstruction_run_id,
            check_run,
            destination,
        }) => commands::reconstruct_run(
            &mut conn,
            reconstruction_run_id,
            check_run,
            destination,
            cli.quiet,
            &policy,
        ),
        TopCommand::Reconstruct(ReconstructCommand::Status {
            reconstruction_run_id,
        }) => commands::reconstruct_status(&conn, reconstruction_run_id),
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
