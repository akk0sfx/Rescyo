# Rescyo のコマンドライン文字列。機械向け出力（JSON）はローカライズしません。ネイティブ校正待ち。

app-about = ディスクに一切書き込まない、無料のオープンソース・データ復旧ツール。

disks-none = ディスクが見つかりません。ディスクの読み取りには管理者または root 権限が必要な場合があります。
disks-size-unknown = サイズ不明（管理者または root 権限が必要）
disks-removable = リムーバブル

scan-start = { $source } をスキャンしています…
phase-probe = ボリュームを検索しています
phase-deep-scan = ソース全体を読み取っています
phase-walk = フォルダーを再構築しています
phase-lost-folders = 失われたフォルダーを配置しています
scan-volume = ボリューム { $id }：{ $fs }、クラスター { $cluster }、ファイル { $files } 個、フォルダー { $dirs } 個、削除済み { $deleted } 個、失われたフォルダー { $lost } 個
scan-volume-skipped = ボリューム { $id }：{ $offset } の { $fs }（スコア { $score }）、未走査
scan-no-volumes = ボリュームが見つかりません。ディープスキャン（--deep）をお試しください。
scan-bad-sectors = 読み取れないセクター { $count } 個をスキップして記録しました。
scan-done = セッションを保存しました：{ $session }

import-summary = DMDE ボリューム { $index }：クラスター { $cluster }、データ開始 { $offset }、ログ上のフォルダー { $folders } 個。ディレクトリ開始点 { $seeds } 個を使用し、クラスター境界から外れた { $offgrid } 個を除外しました。

ls-total = { $count } 件

recover-plan = ファイル { $files } 個、{ $size }、保存先：{ $target }
recover-done = ファイル { $files } 個（{ $size }）を復旧しました。レポート：{ $report }
recover-intact = { $count } 個のファイルを形式どおりに検証し、破損はありませんでした。
recover-damaged = { $count } 個のファイルが形式どおりに読み取れません。レポートを確認してください。
recover-partial = { $count } 個のファイルに、ゼロで埋めた読み取り不能部分があります。レポートを確認してください。
recover-guessed = { $count } 個のファイルは割り当て情報がないため連続しているものとして読み取りました。使用前に確認してください。
recover-resumed = { $count } 個のファイルは前回の実行で保存先にあり、検証済みです。
recover-failed = { $count } 個のファイルを書き込めませんでした。レポートを確認してください。

info-source = ソース：{ $path }
info-source-unverified = セッションは指紋が一致しないソースに接続されています。結果は検証されていません。
info-volumes = ボリューム：{ $count }
info-entries = エントリ：{ $count }

problem-target-on-source-device = 保存先がソースと同じ物理ディスク上にあります（{ $devices }）。別のディスク上のフォルダーを選んでください。
problem-unverified-target = 保存先がどのディスク上にあるか判別できません（{ $reason }）。別のフォルダーを選ぶか、別のディスクだと確信できる場合は --allow-unverified-target を使用してください。
problem-source-devices-unknown = ソースがどのディスク上にあるか判別できません（{ $reason }）。
problem-insufficient-space = 空き容量が不足しています：必要 { $needed }、空き { $free }。
problem-file-too-large = { $path } は { $size } で、保存先のファイルシステムの上限（{ $limit }）を超えています。exFAT または NTFS の保存先を使用してください。
problem-nothing-selected = 条件に一致する項目がありません。

error-source-open = { $source } を開けません：{ $reason }
error-image-unsupported = { $source } を開けません：{ $reason }。raw イメージに変換して（例：qemu-img）から開いてください。
error-parent-missing = { $source } は差分イメージですが、親イメージが見つかりません（試したパス：{ $tried }）。親イメージを同じ場所に置いてください。
error-source-mismatch = { $source } はこのセッションを作成したソースではありません（{ $reason }）。確信がある場合のみ --force-rebind を使用してください。
error-session-exists = { $path } は既に存在します。Rescyo はセッションを上書きしません。別のファイル名を指定してください。
error-session-invalid = { $path } は Rescyo のセッションではありません：{ $reason }
error-session-on-source = セッションファイルがソースのディスク上になります（{ $devices }）。別のディスクに保存してください。
error-degraded = ソースが劣化しています：読み取れないセクター { $count } 個。すぐに中止し、先にイメージを作成してから（例：GNU ddrescue）イメージをスキャンしてください。それでも続ける場合は --continue-on-errors を使用してください。
error-gone = ソースが切断されました：{ $reason }
error-dmde-parse = DMDE ログを読み取れません：{ $reason }
error-dmde-geometry = DMDE ログは別のディスク（{ $log }）を記述しており、ソースは { $source } です。確信がある場合のみ --force を使用してください。
error-internal = 内部エラー：{ $reason }
error-cancelled = 中止しました。ここまでに見つかった内容はセッションに保存されています。
error-preflight = 何も書き込んでいません。先に次の問題を解決してください：

hint-admin = ディスクの読み取りには、Windows では管理者権限、Linux と macOS では sudo が必要です。

## Imaging
image-start = { $source } のイメージを { $image } に作成しています（マップ：{ $map }）…
container-opened = { $format } イメージ：仮想ディスク { $size }、ファイル数 { $files }。
container-note = イメージに関する注意：{ $note }
phase-image-copy = コピー
phase-image-trim = 不良領域のトリミング
phase-image-scrape = 不良領域の再読み取り
phase-image-retry = 不良セクタの再試行
image-rescued = 救出済み
image-bad-short = 不良
image-done = イメージ完成：{ $size } のうち { $rescued } を救出しました。
image-bad = { $count } 個の不良領域（{ $size }）を読み取れませんでした。イメージ内ではゼロになり、マップに記録されています。イメージをスキャンするときは --map を指定してください。
error-image-exists = { $path } は既に存在し、マップがありません。Rescyo はイメージを上書きしません。別のファイル名を選ぶか、その --map を指定してください。
error-map-invalid = マップ { $path } を読み取れません：{ $reason }
error-map-mismatch = マップのディスクサイズは { $map } ですが、ソースは { $source } です。
error-image-cancelled = 停止しました。マップは保存されています。同じコマンドを再実行すると続行します。
hint-image-resume = マップは保存されています。ディスクを再接続し、同じコマンドを再実行すると続行します。

## Partition search
partitions-start = { $source } のブートセクタを検索しています…
partitions-table = パーティションテーブル：{ $table }
partitions-none = ボリュームは見つかりませんでした。
partition-listed = #{ $number }（{ $offset }）：{ $fs }{ $label }、{ $size }、テーブルに記載あり（{ $boot }）
partition-lost = 失われたパーティション（{ $offset }、バイト { $bytes }）：{ $fs }{ $label }、{ $size }、テーブルに記載なし（{ $boot }）
partition-conflicting = ボリューム（{ $offset }、バイト { $bytes }）：{ $fs }{ $label }、{ $size }、テーブルの項目と重複（{ $boot }）
partition-nested = 別のボリューム内のボリューム（{ $offset }）：{ $fs }{ $label }、{ $size }（{ $boot }）
partition-empty = #{ $number }（{ $offset }、{ $size }）：先頭にボリュームが見つかりません
partition-boot-both = ブートセクタとバックアップ
partition-boot-primary = ブートセクタ
partition-boot-backup = バックアップのブートセクタのみ
partition-boot-none = ブートセクタなし
partition-unpartitioned = パーティションテーブルなし：ディスク全体が 1 つのボリューム、{ $fs }{ $label }、{ $size }（{ $boot }）
scan-carved = ボリューム { $id }：シグネチャで { $files } 個のファイルを検出（種類別のフォルダーに格納、復旧時に内容を検証）
