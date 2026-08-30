# DBスキーマ案（検討中）

`docs/open-decisions.md` の **2.1 SQLiteスキーマ設計** に対する検討案。
まだ `docs/requirements.md` には確定事項として反映していない、レビュー用のドラフト。

前提となる決定事項: `docs/requirements.md` 10.1〜10.6（ハッシュアルゴリズム、重複判定方式、
処理フェーズ分離、リムーバブルメディア識別、圧縮ファイル対応・安全対策）。

## 全体構成（ER概要）

```
scan_session ──< scan_session_target >── removable_media
      │
      └──< scanned_file (自己参照でアーカイブ内ネストを表現)
                  │
                  ├──< integrity_check_result >── reference_file ── reference_set
                  │
                  └──< duplicate_group_member >── duplicate_group

app_setting (キー・バリューの設定値)
```

## 1. スキャン実行の管理

### scan_session

| 列 | 型 | 備考 |
|---|---|---|
| id | INTEGER PK | |
| check_type | TEXT | `integrity` / `duplicate` |
| started_at | TEXT(ISO8601) | |
| completed_at | TEXT(ISO8601) NULL | |
| status | TEXT | `running` / `completed` / `failed` |

### scan_session_target

1セッションが複数フォルダ・複数リムーバブルメディアを横断できるようにするための junction テーブル。

| 列 | 型 | 備考 |
|---|---|---|
| id | INTEGER PK | |
| scan_session_id | INTEGER FK → scan_session.id | |
| target_type | TEXT | `folder` / `removable_media` |
| folder_path | TEXT NULL | target_type=folder のとき |
| removable_media_id | INTEGER FK NULL → removable_media.id | target_type=removable_media のとき |

**理由**: 10.3で決めた「情報取得フェーズ→比較フェーズ」の分離を素直に落とすと、`scan_session`が
1回の情報取得実行を表す単位になる。重複チェックは複数フォルダ・複数メディアを横断するのが要件
（3.2）なので、セッション:対象を1:Nにしないと表現できない。「リムーバブルメディアは接続時に
スキャンし、以降は再接続なしで過去結果を再利用」という要件も、`scan_session_target`経由で
「このメディアの最新セッションはどれか」を引けば実現できる。

## 2. リムーバブルメディア識別（10.4準拠）

### removable_media

| 列 | 型 | 備考 |
|---|---|---|
| id | INTEGER PK | |
| platform | TEXT | `windows` / `macos` / `linux` |
| identifier_type | TEXT | 例: `device_serial`, `filesystem_uuid` |
| identifier_value | TEXT | |
| display_name | TEXT NULL | ユーザー向け表示名（ボリュームラベル等） |
| first_seen_at | TEXT | |
| last_seen_at | TEXT | |

UNIQUE制約: `(platform, identifier_type, identifier_value)`

**理由**: 10.4の「OS固有フィールドを持たず`identifier_type`+`identifier_value`+`platform`に抽象化」
をそのまま列にしている。フォールバック方針（2.3、未決）は将来`confidence`列や
`is_fallback_match`フラグを追加する余地を残す設計とし、今回は追加しない。

## 3. ファイル情報（走査結果・ハッシュ）

### scanned_file

| 列 | 型 | 備考 |
|---|---|---|
| id | INTEGER PK | |
| scan_session_id | INTEGER FK → scan_session.id | |
| path | TEXT | フォルダ/メディアルートからの相対パス |
| parent_archive_file_id | INTEGER FK NULL → scanned_file.id | アーカイブ内ファイルの場合、親アーカイブの行 |
| archive_format | TEXT NULL | 例: `zip`, `7z`。自由文字列、CHECK制約なし |
| archive_depth | INTEGER | 0=通常ファイル、1〜3=ネスト段数 |
| size | INTEGER | |
| mtime | TEXT NULL | |
| crc32 | TEXT NULL | |
| md5 | TEXT NULL | |
| sha1 | TEXT NULL | |
| sha256 | TEXT NULL | |
| status | TEXT | `ok` / `error` / `skipped` |
| error_message | TEXT NULL | |
| scanned_at | TEXT | |

**インデックス方針**:
- `(scan_session_id)`
- `(size)` — 重複チェック第1段階の絞り込み
- `(size, crc32)` — 第2段階の絞り込み
- `(sha256)` — 最終確定・整合性チェックの照合
- `(parent_archive_file_id)`

**理由・ポイント**:
- **ハッシュを正規化テーブルにせず列で持つ**: `file_hash(scanned_file_id, algorithm, value)`の
  ような正規化案も検討したが、非機能要件（数十万〜百万ファイル規模での速度優先）を考えると、
  重複チェックのグルーピングは `GROUP BY size` → `GROUP BY size, crc32` → `GROUP BY sha256` が
  主要クエリになる。正規化するとJOINが挟まりこの規模では不利なため、列持ちを採用。
- **各ハッシュ列はNULL許容**: 10.3で「比較フェーズでハッシュ計算する」と決めており、重複チェックは
  段階的（サイズ→CRC32→SHA256）にしか計算しないため、多くのファイルはCRC32までしか埋まらない。
  整合性チェックも外部お手本セット側が提供するアルゴリズムのみ埋まればよい（10.1）。全列NULL許容
  にして「どこまで計算が進んだか」を表現する。
- **`parent_archive_file_id`の自己参照でアーカイブネストを表現**: 10.6の「再帰展開は最大3階層まで」
  という制約が、`archive_depth`列の値域チェック（アプリ層で0〜3に制限）としてそのまま落とし込める。
  別テーブルにせず`scanned_file`に含めているのは、アーカイブ内ファイルも通常ファイルも
  「重複チェック・整合性チェックの対象」という点で本質的に同種のレコードだから。
- **`archive_format`は自由文字列**: 10.5の決定通り、CHECK制約を付けずアプリ層で対応形式を判定する。

## 4. 整合性チェック（お手本セット照合）

### reference_set

| 列 | 型 | 備考 |
|---|---|---|
| id | INTEGER PK | |
| name | TEXT | |
| source_format | TEXT | `json` / `csv` / `xml` 等 |
| created_at | TEXT | |

### reference_file

| 列 | 型 | 備考 |
|---|---|---|
| id | INTEGER PK | |
| reference_set_id | INTEGER FK → reference_set.id | |
| path | TEXT | |
| size | INTEGER | |
| crc32 / md5 / sha1 / sha256 | TEXT NULL | 各アルゴリズムごとに独立カラム、提供されたものだけ埋まる |

### integrity_check_result

| 列 | 型 | 備考 |
|---|---|---|
| id | INTEGER PK | |
| scan_session_id | INTEGER FK → scan_session.id | |
| reference_set_id | INTEGER FK → reference_set.id | |
| reference_file_id | INTEGER FK NULL → reference_file.id | |
| scanned_file_id | INTEGER FK NULL → scanned_file.id | |
| result_status | TEXT | `ok` / `corrupted` / `missing` / `extra` |
| detail | TEXT NULL | |

**理由**: お手本セット自体（`reference_set`/`reference_file`）と走査結果（`scanned_file`）を分離し、
両者を突き合わせた結果だけを`integrity_check_result`に持たせる。同じお手本セットに対して複数回
チェックを実行しても`reference_set`は使い回せ、外部形式（CSV/XML）のフィールドマッピング仕様
（3.1、未決）を後で拡張しても`reference_file`のスキーマ自体は変わらない。

## 5. 重複チェック結果

### duplicate_group

| 列 | 型 | 備考 |
|---|---|---|
| id | INTEGER PK | |
| sha256 | TEXT | 確定ハッシュ |
| size | INTEGER | |
| member_count | INTEGER | |

### duplicate_group_member

| 列 | 型 | 備考 |
|---|---|---|
| id | INTEGER PK | |
| duplicate_group_id | INTEGER FK → duplicate_group.id | |
| scanned_file_id | INTEGER FK → scanned_file.id | |

**理由**: グループ:ファイルが1:Nの関係なので中間テーブルで正規化。`duplicate_group`に確定ハッシュ
(SHA-256)とサイズを持たせることで、GUI一覧表示時に毎回集計しなくて済む。

## 6. 横断設定

### app_setting（key-value）

| 列 | 型 | 備考 |
|---|---|---|
| key | TEXT PK | 例: `archive_max_depth`, `archive_entry_size_limit_bytes` |
| value | TEXT | |

**理由**: 10.6で「深さ制限・サイズ上限はハードコードせず設定値として保持」と決めているため、専用
テーブルより素朴なkey-valueで十分。

## あえて含めなかったもの

- **パスワード保存**（6.4、未決）: DBに平文で持つべきでない方針が決まっているため、テーブル設計は
  行わない。キーチェーン等の外部ストア参照キーだけをDBに持つ形になる想定だが、保存方式自体が未決
  のため保留。
- **リムーバブルメディアのフォールバック識別**（2.3、未決）: `removable_media`テーブルに信頼度
  カラムを足す程度の拡張で対応できる想定だが、今回は追加しない。

## 未確定・レビュー待ちの点

- ハッシュ列を正規化せず`scanned_file`に直接持たせる方針の是非
- アーカイブネストを別テーブルにせず自己参照で表現する方針の是非
- `integrity_check_result` / `duplicate_group`系のステータス値・列構成の粒度

この案がレビューされ確定したら、`docs/requirements.md` に「10.8 SQLiteスキーマ設計」として
反映し、`docs/open-decisions.md` の2.1にチェックを入れる。
