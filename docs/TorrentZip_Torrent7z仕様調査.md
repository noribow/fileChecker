# TorrentZip / Torrent7z 仕様調査

6.3（お手本セットに合わせた再構成〔圧縮ファイル書き出し〕機能の詳細仕様）の前提資料として、
ユーザーから提示された以下2点のPDFの内容を整理したもの。10.5で「再構成時はTorrentZip（zip向け）・
Torrent7z（7z向け）に準拠したファイル構造で出力する」ことは決定済みだが、6.3自体（トリガー条件・
対象範囲）は引き続き未決事項として残る（`docs/open-decisions.md` 6.3参照）。本ドキュメントは
フォーマット仕様側の調査結果であり、6.3の意思決定そのものを行うものではない。

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
またはRomVault/T7z関連の実装ソースコード）が必要になる。
