# TorrentZip / Torrent7z 仕様調査

6.3（お手本セットに合わせた再構成〔圧縮ファイル書き出し〕機能の詳細仕様）の前提資料として、
ユーザーから提示された以下2点のPDFの内容を整理したもの。10.5で「再構成時はTorrentZip（zip向け）・
Torrent7z（7z向け）に準拠したファイル構造で出力する」ことは決定済みで、本調査（3章）で浮上した
「Torrent7zをレガシー形式・後継形式（RV7Z）のどちらの意味で満たすか」という論点は、RV7Zを採用する
ことで決定済み（`docs/requirements.md` 10.19参照）。6.3自体の残り（トリガー条件・対象範囲・CLI/GUIの
起動導線）も§10.20で決定済み（`docs/open-decisions.md` 6.3参照）。非Solid版・ZSTD版7zへの対応要否と
再構成先での同名別内容ファイルの扱いのみ、`docs/open-decisions.md` 7.3・7.4として引き続き未決のまま残る。

出典:
- TorrentZip Implementation Standards, by Gordon J（<https://www.romvault.com/trrntzip_explained.pdf>）
- Understanding 7z Compression File Format, by gordon@romvault.com
  （<https://www.romvault.com/Understanding7z.pdf>）

※この環境からは上記romvault.comドメインへのネットワークアクセスがプロキシでブロックされているため、
  ユーザーがアップロードしたPDFファイルの内容を読み取って記載している。

## 1. TorrentZip（zip向け）— 仕様は具体的かつ完結している

決定的（同一内容から生成すれば常にバイト一致する）なzipを生成するための、実装レベルで
完結した仕様。以下をすべて満たす必要がある。

### 1.1 全体構造

通常のzipと同じ並び:

```
[local file header 1][file data 1]
[local file header 2][file data 2]
...
[local file header n][file data n]
  ← SOCD (start of central directory)
[central directory file 1]...[central directory file n]
  ← EOCD (end of central directory)
[end of central directory record]
```

### 1.2 ローカルファイルヘッダ（固定値）

| フィールド | 固定値 | 備考 |
|---|---|---|
| Version needed to extract | 20 | Deflate圧縮を使用 |
| General purpose bit flag | 2 | 最大圧縮オプション使用を示す |
| Compression method | 8 | Deflate |
| Last mod file time | 48128 (23:32) | 固定タイムスタンプ |
| Last mod file date | 8600 (1996/12/24) | MAME初リリース日。全エントリで固定 |
| Extra field length | 0 | 拡張フィールドなし |

CRC-32・圧縮後サイズ・展開後サイズ・ファイル名長はファイルごとの実値。

### 1.3 セントラルディレクトリ（固定値）

ローカルファイルヘッダと同じ固定値（Version needed to extract=20、bit flag=2、
compression method=8、time=48128、date=8600、Extra field length=0）に加え:

| フィールド | 固定値 |
|---|---|
| Version made by | 0（MS-DOS/OS2, FAT/FAT32） |
| File comment length | 0 |
| Disk number start | 0 |
| Internal file attributes | 0 |
| External file attributes | 0 |

### 1.4 圧縮アルゴリズム

**zlib version 1.1.3 相当・圧縮レベル9（最大圧縮）**で決定的に圧縮する。zlibの実装・バージョンに
よってDeflate出力バイト列が変わり得るため、「バイト一致」を要件とするなら圧縮出力そのものの
互換性検証が必要になる（現行の主要言語のzlib/miniz実装で1.1.3と同一バイト列を再現できるかは
別途確認が必要）。

### 1.5 EOCDのZIPファイルコメント（整合性検証用マーカー）

- 長さ22バイト固定、内容は `TORRENTZIPPED-XXXXXXXX`
- `XXXXXXXX` は、central directoryの先頭（SOCD）から末尾（EOCD直前）までのバイト列に対する
  CRC32を16進大文字テキストにしたもの
- 生成側はファイル内容が変われば必ずこの値が実際のcentral directoryバイト列と食い違うようになり、
  「このzipがtorrentzip仕様に沿って生成されたか」を検証できる

### 1.6 エントリの並び順

**ファイル名の小文字化ソート（lower-case sort）順**で並べる。

### 1.7 パス区切り文字

`\` は必ず `/` に正規化する。**ソートより前に**正規化を行うこと（正規化後の文字列でソートしないと
順序が変わってしまうため）。

### 1.8 ディレクトリエントリ・空ディレクトリの扱い

- ディレクトリは末尾が `/` でサイズ0・CRC0のエントリとして表現する
- あるファイルパス（例: `set1/test1.rom`）が存在する場合、そこから暗示される親ディレクトリ
  エントリ（`set1/`）は**不要なので取り除く**
- 逆に、どのファイルからも暗示されない空ディレクトリ（配下にファイルが1つもない）は、
  その空ディレクトリを表現するためのゼロサイズエントリとして**保持する**

### 1.9 重複ファイル名の検出

zip内に同名エントリが複数存在するケースをチェックし、検出時は警告することが望ましいとされている
（多くのzip実装は重複エントリを暗黙に無視してしまうため）。

---

## 2. Torrent7z（7z向け）— 提示資料はTorrent7z固有の決定的仕様を含まない

`Understanding7z.pdf` は、タイトルの通り**汎用の7z圧縮ファイルフォーマットの内部データ構造**
（Folder / Coder / BindPair / PackedStreamsInfo などの関係）を解説した文書であり、
「Torrent7z」という名称や、TorrentZipに相当する決定的生成のための固定パラメータ
（圧縮方式・辞書サイズ・ソート順・固定タイムスタンプ・検証用マーカー等）には言及していない。
また文書末尾の「7z File Structure」節はバイトレベルの説明が `InProgress`（未完成）のまま
終わっている。

このPDFから確認できる内容は、あくまで7zの一般的な内部構造の理解に資する以下の情報にとどまる。

- 7zは複数の入力ファイルを1つの連結ストリームとして扱い、圧縮フィルタ（LZMA・BCJ2等）を
  チェーン接続できる（この連結・チェーン機構が「Folder」）
- ヘッダは `FileInfo`（ファイル名・空ファイル/ディレクトリフラグ）と `StreamsInfo`
  （`PackPosition` / `PackedStreamsInfo[]` / `Folders[]`）の2要素からなる
- `Folder` は `Coder[]`（使用する圧縮方式・入出力ストリーム数・設定バイト列）、
  `BindPair[]`（コーダ間の出力→入力の接続）、`PackedStreamIndicies[]`
  （パック済みストリームと未接続入力の対応）、`UnpackedStreamSizes` /
  `UnpackCRC` / `UnpackedStreamInfo[]`（展開後の各ファイルのサイズ・CRC）を持つ
- ファイルを追加する際は既存ストリームを伸長・再圧縮するのではなく、新規ファイル群ごとに
  新しい `Folder` を追加するのが一般的な実装（差分追加のための機構）

### 残課題

Torrent7z（T7z）としてバイト一致の決定的出力を得るために必要な、以下のような固定パラメータ・
規則は、提示された資料には含まれておらず、**別途情報源が必要**:

- 圧縮方式の固定（LZMA/LZMA2のどちらか、辞書サイズ、圧縮レベル等のエンコーダ設定）
- solid圧縮の可否・folder分割方針（1 folder固定か、ファイルごとに分けるか）
- ファイルエントリの並び順規則（TorrentZipの「小文字ソート」に相当する規則があるか）
- タイムスタンプ・属性の扱い（TorrentZipの固定日時1996/12/24 23:32相当の規則があるか）
- 生成ファイルがTorrent7z準拠であることを検証するためのマーカー・ハッシュの有無
  （TorrentZipのEOCDコメント `TORRENTZIPPED-XXXXXXXX` に相当するもの）
- 通常の7zエンコーダ（7-Zip公式等）でこれらの固定パラメータを指定すれば足りるのか、
  専用の実装（RomVaultのTorrent7z実装等）のソースを参照する必要があるのか

6.3の詳細仕様検討を再開する際は、上記の残課題を埋める追加資料（Torrent7z専用の仕様文書、
またはRomVault/T7z関連の実装ソースコード）が必要になる。→ 3章で後述するRomVault/RVWorld実装の
調査により、この残課題の大部分は解消した。

---

## 3. 実装リファレンス調査: RomVault/RVWorld

`docs/DBスキーマ案.md`が参照する形式（softwarelist.dtd/mame.dtd）と同じ作者・エコシステムの
実装として、RomVaultの後継ツール群である [RomVault/RVWorld](https://github.com/RomVault/RVWorld)
（Apache License 2.0、参照・実装移植とも利用可）の
`Compress/StructuredZip`（`Structured7Zip.cs` / `StructuredZip.cs` / `StructuredArchive.cs`）と
`Compress/SevenZip/SevenZipWrite.cs`、`libraries/SortMethods/Sorters.cs` を調査した
（2026-09-04時点のmasterブランチ、コミット `d0d6f6b`）。TorrentZipの仕様書（1章）を補強する
情報と、Torrent7z側で不足していた決定的生成パラメータ（2章の残課題）の大部分がここから判明した。

### 3.1 重要な発見: 「Torrent7z」はレガシー形式であり、RomVaultの現行実装は独自の後継形式に移行している

`StructuredArchive.cs` の `ZipStructure` enumには次のように明記されている:

```csharp
SevenZipTrrnt = 4,  // this is the original t7z format
SevenZipSLZMA = 8,  // Solid-LZMA this is rv7zip today
SevenZipNLZMA = 9,  // NonSolid-LZMA
SevenZipSZSTD = 10, // Solid-zSTD
SevenZipNZSTD = 11, // NonSolid-zSTD
```

- `SevenZipTrrnt`（オリジナルの"Torrent7z" = `torrent7z_0.9beta`）は**読み取り（検出）のみ**
  実装されており（`Istorrent7Z()`）、RVWorldはこの形式で新規に書き出すことをしない。
  検証方法は、7zファイルの末尾付近に埋め込まれた固定シグネチャ文字列
  `"torrent7z_0.9beta"`（先頭に固定のXORキー的なマジックバイト列を伴う）と、そのブロックの
  CRC32が一致するかで判定する、独自の後付け検証方式。
- RomVaultが**現在標準として書き出す**のは `SevenZipSLZMA`（コメントで
  "this is rv7zip today" と明記）を筆頭とする**独自の"RomVault7Zip"形式**であり、
  厳密には元祖Torrent7zの仕様を継承していない。検証マーカーも、末尾に
  `"RomVault7Z0" + バリアント番号1文字（'1'〜'4'）` の12バイト固定シグネチャ＋
  ヘッダCRC(4B)＋ヘッダ位置(8B)＋ヘッダ長(8B)を追記する独自方式（`WriteRomVault7Zip()`）。

**この論点は2026-09-04に決定した**（`docs/requirements.md` 10.19参照）: requirements.md 10.5の
「Torrent7z（7z向け）に準拠したファイル構造で出力する」は、元祖`torrent7z_0.9beta`形式ではなく、
RomVaultが実運用で現に採用している後継形式（RomVault7Zip系、以下便宜上「RV7Z」）に倣うものとする。

### 3.2 RV7Z（RomVaultの現行7z決定的生成方式）の具体的パラメータ

`SevenZipSLZMA`（Solid-LZMA、"today"のデフォルト）を例に、決定的出力に必要な固定パラメータが
すべて実装から読み取れる:

| 項目 | 値・規則 |
|---|---|
| 圧縮方式 | LZMA（7zip method ID `03,01,01`）固定。ZSTD版（`SevenZipSZSTD`/`NZSTD`、method ID `04,F7,11,01`）も選択肢として存在 |
| solid圧縮 | Solid版（`SLZMA`/`SZSTD`）は全ファイルを1つのfolder（1ストリーム）にまとめて圧縮。Non-Solid版（`NLZMA`/`NZSTD`）はファイルごとに個別folder |
| 辞書サイズ | ハードコードされた固定値ではなく、**総展開後サイズ（solid）またはファイルごとの展開後サイズ（non-solid）を、`{0x10000, 0x18000, 0x20000, ... , 0x4000000, 0x6000000}`という22段階の昇順テーブルから、その値以上になる最小の段階を選んで決定**する関数（`GetDictionarySizeFromUncompressedSize`）。上限は0x6000000（96MiB）で、これを超える場合は最大値（96MiB）に丸められる |
| LZMA numFastBytes | 64固定 |
| タイムスタンプ | 7zヘッダに**一切書き込まない**（`ZipFileOpenWriteStream`に`modTime`引数はあるが7z書き込み経路では未使用）。TorrentZipの「固定日時を書き込む」方式とは異なり、そもそも該当プロパティ自体を省略する方式 |
| ファイル並び順 | `Trrnt7ZipStringCompare`：**拡張子→ファイル名（拡張子除く）→ディレクトリパス**の順に、いずれも大文字小文字を区別する通常の（ordinal）文字列比較で比較する、3段階のソートキー。TorrentZip（zip側）の「パス全体を小文字化して比較」というルールとは全く異なる規則である点に注意 |
| 検証マーカー | 末尾に `RomVault7Z0` + バリアント番号1桁（'1'=SLZMA/'2'=NLZMA/'3'=SZSTD/'4'=NZSTD）+ ヘッダCRC(4B)+ヘッダ位置(8B)+ヘッダ長(8B)。7z標準のsignature headerが持つ`NextHeaderCRC`/`NextHeaderLocation`/`NextHeaderSize`と再度一致するかで検証する |

### 3.3 zip側（TorrentZip）: 実装で追加確認できた事項

`StructuredZip.cs`／`Sorters.cs`により、1章のPDF記載内容を補強・訂正できる点:

- **ソート順の正確なアルゴリズム**: 単純な「文字列を小文字化してソート」ではなく、
  1文字ずつ比較し、ASCII A-Z（0x41-0x5A）の範囲のみ+0x20して小文字化してから比較する
  `TrrntZipStringCompare`。これが等しい場合のみ、元の大文字小文字を保持したままの
  ordinal比較で確定させる（大文字小文字違いの同名ファイルがあり得るための2段階比較）。
  PDFの「lower case sort」という説明は結果的に近いが、厳密な実装はこの2段階アルゴリズム。
- 同じzip実装コードベースで、オリジナルのTorrentZip（`ZipTrrnt`、コメントprefix
  `TORRENTZIPPED-`）以外にも、`TDC`（`TDC-`、Deflate・日時は任意）、`DTD`（`DTD-`、
  コメントマーカーなし）、`ZSTD`（`RVZSTD-`、zstd圧縮・日時なし・ストリーム末尾3バイト
  `01,00,00`で検証）、`DTZ`（`DTZ-`、コメントマーカーなし・zstd）という複数の亜種が
  存在する。ディレクトリエントリの冗長性チェックは`ZipTrrnt`/`ZipZSTD`のみに適用される
  （`TDC`/`DTD`/`DTZ`は対象外）。6.3では、これら亜種のうちどれを採用するか
  （原則はオリジナルの`ZipTrrnt`のみで良いはずだが、念のため記載）も整理対象になる。

### 3.4 6.3の残課題への影響

2章で「Torrent7z専用の追加資料が必要」としていた項目のうち、以下は本調査で解消した:

- 圧縮方式の固定 → 解消（LZMA、method ID `03,01,01`。ZSTD版という代替も存在）
- 辞書サイズ・圧縮レベル等のエンコーダ設定 → 解消（固定テーブルからの動的選択関数、numFastBytes=64）
- solid圧縮の可否・folder分割方針 → 解消（Solid/Non-Solidの2系統が存在し、"today"のデフォルトはSolid）
- ファイルエントリの並び順規則 → 解消（`Trrnt7ZipStringCompare`：拡張子→名前→パス）
- タイムスタンプの扱い → 解消（そもそも書き込まない）
- 検証用マーカーの有無 → 解消（ただし独自の"RomVault7Z"マーカーであり、元祖`torrent7z_0.9beta`
  マーカーとは別物）

一方、新たに浮上した論点（3.1で前述）は決定済み:

- **「Torrent7z」を名乗る2つの異なる実体**（レガシーな`torrent7z_0.9beta`検証形式と、
  RomVaultが現在実際に使っている独自後継形式「RV7Z」）のうち、**RV7Zに準拠する**と決定した
  （2026-09-04、`docs/requirements.md` 10.19参照）。互換性目的（既存のTorrent7z/RV7Z生成物との比較）が
  主眼であり、RomVault自身が新規書き出しをやめているレガシー形式に準拠する実務的メリットがないため。
