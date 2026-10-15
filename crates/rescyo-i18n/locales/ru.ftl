# Строки командной строки Rescyo. Машинный вывод (JSON) не локализуется.

app-about = Бесплатное восстановление данных с открытым кодом, которое никогда не пишет на ваш диск.

disks-none = Диски не найдены. Для чтения дисков могут понадобиться права администратора или root.
disks-size-unknown = размер неизвестен (нужны права администратора или root)
disks-removable = съёмный

scan-start = Сканирование { $source }…
phase-probe = Поиск томов
phase-deep-scan = Чтение всего источника
phase-walk = Восстановление папок
phase-lost-folders = Размещение потерянных папок
scan-volume = Том { $id }: { $fs }, кластер { $cluster }, { $files ->
        [one] { $files } файл
        [few] { $files } файла
       *[many] { $files } файлов
    }, { $dirs ->
        [one] { $dirs } папка
        [few] { $dirs } папки
       *[many] { $dirs } папок
    }, удалённых: { $deleted }, потерянных: { $lost }
scan-volume-skipped = Том { $id }: { $fs } по смещению { $offset } (оценка { $score }), не разобран
scan-no-volumes = Тома не найдены. Попробуйте глубокое сканирование: --deep.
scan-bad-sectors = { $count ->
        [one] { $count } нечитаемый сектор пропущен и записан в карту.
        [few] { $count } нечитаемых сектора пропущены и записаны в карту.
       *[many] { $count } нечитаемых секторов пропущены и записаны в карту.
    }
scan-done = Сессия сохранена: { $session }

import-summary = Том DMDE { $index }: кластер { $cluster }, данные с { $offset }, папок в логе: { $folders }. Использовано начал каталогов: { $seeds }, отброшено вне сетки кластеров: { $offgrid }.

ls-total = { $count ->
        [one] { $count } запись
        [few] { $count } записи
       *[many] { $count } записей
    }

recover-plan = { $files ->
        [one] { $files } файл
        [few] { $files } файла
       *[many] { $files } файлов
    }, { $size }, в { $target }
recover-done = Восстановлено { $files ->
        [one] { $files } файл
        [few] { $files } файла
       *[many] { $files } файлов
    } ({ $size }). Отчёт: { $report }
recover-intact = Проверено по формату и целы: { $count }.
recover-damaged = { $count ->
        [one] { $count } файл не читается как свой формат; см. отчёт.
        [few] { $count } файла не читаются как свой формат; см. отчёт.
       *[many] { $count } файлов не читаются как свой формат; см. отчёт.
    }
recover-partial = { $count ->
        [one] У { $count } файла есть нечитаемые части, заполненные нулями; см. отчёт.
       *[other] У { $count } файлов есть нечитаемые части, заполненные нулями; см. отчёт.
    }
recover-guessed = { $count ->
        [one] { $count } файл прочитан как непрерывный, без данных о размещении. Проверьте его, прежде чем доверять.
        [few] { $count } файла прочитаны как непрерывные, без данных о размещении. Проверьте их, прежде чем доверять.
       *[many] { $count } файлов прочитаны как непрерывные, без данных о размещении. Проверьте их, прежде чем доверять.
    }
recover-resumed = Уже были в папке назначения с прошлого запуска и проверены: { $count }.
recover-failed = Не удалось записать файлов: { $count }; см. отчёт.

info-source = Источник: { $path }
info-source-unverified = Сессия привязана к источнику, отпечаток которого не совпал. Результаты не подтверждены.
info-volumes = Томов: { $count }
info-entries = Записей: { $count }

problem-target-on-source-device = Папка назначения на том же физическом диске, что и источник ({ $devices }). Выберите папку на другом диске.
problem-unverified-target = Rescyo не может определить, на каком диске папка назначения ({ $reason }). Выберите другую папку или используйте --allow-unverified-target, если уверены, что это другой диск.
problem-source-devices-unknown = Rescyo не может определить, на каком диске источник ({ $reason }).
problem-insufficient-space = Недостаточно места: нужно { $needed }, свободно { $free }.
problem-file-too-large = { $path } весит { $size } — больше, чем допускает файловая система назначения ({ $limit }). Используйте диск в exFAT или NTFS.
problem-nothing-selected = Под выбор ничего не попало.

error-source-open = Не удалось открыть { $source }: { $reason }
error-image-unsupported = Не удалось открыть { $source }: { $reason }. Преобразуйте его в сырой образ (например, через qemu-img) и откройте его.
error-parent-missing = { $source } — разностный образ, а его родительский образ не найден (искал: { $tried }). Положите родительский образ рядом.
error-source-mismatch = { $source } — не тот источник, по которому сделана сессия ({ $reason }). Используйте --force-rebind, только если уверены.
error-session-exists = { $path } уже существует. Rescyo никогда не перезаписывает сессию; выберите другое имя файла.
error-session-invalid = { $path } — не сессия Rescyo: { $reason }
error-session-on-source = Файл сессии оказался бы на исходном диске ({ $devices }). Сохраните его на другом диске.
error-degraded = Источник деградирует: нечитаемых секторов { $count }. Остановитесь и сначала снимите образ (например, GNU ddrescue), затем сканируйте образ. Чтобы продолжить всё равно, используйте --continue-on-errors.
error-gone = Источник пропал: { $reason }
error-dmde-parse = Не удалось прочитать лог DMDE: { $reason }
error-dmde-geometry = Лог DMDE описывает другой диск ({ $log }), а источник — { $source }. Используйте --force, только если уверены.
error-internal = Внутренняя ошибка: { $reason }
error-cancelled = Остановлено. Всё найденное сохранено в сессии.
error-preflight = Ничего не записано. Сначала устраните проблемы:

hint-admin = Для чтения дисков нужны права администратора в Windows или sudo в Linux и macOS.

## Imaging
image-start = Снимаю образ { $source } в { $image } (карта: { $map })…
container-opened = Образ { $format }: виртуальный диск { $size }, файлов: { $files }.
container-note = Замечание об образе: { $note }
phase-image-copy = Копирование
phase-image-trim = Обрезка плохих зон
phase-image-scrape = Дочитка плохих зон
phase-image-retry = Повтор плохих секторов
image-rescued = спасено
image-bad-short = плохих
image-done = Образ готов: спасено { $rescued } из { $size }.
image-bad = { $count ->
        [one] { $count } плохая зона ({ $size }) не прочиталась.
        [few] { $count } плохие зоны ({ $size }) не прочитались.
       *[many] { $count } плохих зон ({ $size }) не прочитались.
    } В образе на их месте нули, в карте они отмечены; при скане образа укажите --map.
error-image-exists = { $path } уже существует, а карты к нему нет. Rescyo никогда не перезаписывает образ; выберите другое имя или укажите его --map.
error-map-invalid = Не удаётся прочитать карту { $path }: { $reason }
error-map-mismatch = Карта описывает диск размером { $map }, а у источника { $source }.
error-image-cancelled = Остановлено. Карта сохранена; запустите ту же команду, чтобы продолжить.
hint-image-resume = Карта сохранена. Подключите диск снова и запустите ту же команду, чтобы продолжить.

## Partition search
partitions-start = Ищу загрузочные секторы на { $source }…
partitions-table = Таблица разделов: { $table }
partitions-none = Тома не найдены.
partition-listed = №{ $number } по смещению { $offset }: { $fs }{ $label }, { $size }, есть в таблице ({ $boot })
partition-lost = Потерянный раздел по смещению { $offset } (байт { $bytes }): { $fs }{ $label }, { $size }, нет в таблице ({ $boot })
partition-conflicting = Том по смещению { $offset } (байт { $bytes }): { $fs }{ $label }, { $size }, пересекается с разделом из таблицы ({ $boot })
partition-nested = Том внутри другого тома по смещению { $offset }: { $fs }{ $label }, { $size } ({ $boot })
partition-empty = №{ $number } по смещению { $offset }, { $size }: в его начале том не найден
partition-boot-both = загрузочный сектор и копия
partition-boot-primary = загрузочный сектор
partition-boot-backup = только резервная копия загрузочного сектора
partition-boot-none = нет загрузочного сектора
partition-unpartitioned = Таблицы разделов нет: весь диск — один том, { $fs }{ $label }, { $size } ({ $boot })
scan-carved = Том { $id }: { $files ->
        [one] { $files } файл найден
        [few] { $files } файла найдено
       *[many] { $files } файлов найдено
    } по сигнатурам (в папках по типу; содержимое проверяется при восстановлении)

## Terminal interface
tui-step-source = Источник
tui-step-scan = Скан
tui-step-browse = Обзор
tui-step-recover = Восстановление
tui-search = Поиск среди { $count } файлов…  (/)
tui-fact-cluster = кластер
tui-fact-data = данные с
tui-fact-folders = папок
tui-fact-files = файлов
tui-col-name = Имя
tui-col-size = Размер
tui-col-modified = Изменён
tui-col-assembly = Сборка
tui-col-integrity = Целостность
tui-intact = Цел
tui-partial = Частично
tui-damaged = Повреждён
tui-unverified = Не проверен
tui-empty = Здесь нет файлов.
tui-insp-runs = Фрагментов
tui-insp-state = Состояние
tui-insp-kind = Тип
tui-deleted = удалён
tui-live = на месте
tui-stream = именованный поток
tui-note-lost = Папок без родителя: { $count }, они в _LOST_FOLDERS.
tui-scanning = Сканирую { $source }
tui-recover-to = Восстановить в
tui-no-target = нажмите T и выберите папку на другом диске
tui-preflight-ok = Проверка: другой диск · свободно { $free } · имена в порядке
tui-preflight-problems = Проблем: { $count }. { $first }
tui-recover-button = R  Восстановить файлов: { $count }
tui-input-target = Папка для восстановления
tui-input-hint = Папка на другом диске. Enter — подтвердить, Esc — отмена.
tui-running = Восстанавливаю… { $done } из { $total }  (Esc — остановить)
tui-cancelled = Остановлено. Уже записанные файлы целы и перечислены в отчёте.
tui-source-changed = Источник больше не совпадает с этой сессией. Если вы уверены, воспользуйтесь командной строкой с --force-rebind.
tui-help-title = Клавиши
tui-help =
    ↑ ↓ или j k      перемещение       Tab        папки или файлы
    → ← или Enter    открыть, закрыть  Пробел     отметить файл   a   отметить все здесь
    /                поиск             d          показать или скрыть удалённые
    s                именованные потоки  t        выбрать папку для восстановления
    r                восстановить      q или Esc  выход
tui-keys =  Tab панели · Пробел отметить · / поиск · d удалённые · t папка · r восстановить · ? справка · q выход

## Desktop interface
gui-sources = Источники
gui-images = Образы
gui-sessions = Сессии
gui-no-target = Выберите папку на другом диске
gui-image-first = Сначала снять образ
gui-empty-title = Вернём всё.
gui-empty-hint = Выберите слева диск, образ или сессию. Rescyo только читает.
gui-open-image = Открыть образ…
gui-open-session = Открыть сессию…
gui-deep = Глубокое сканирование: прочитать весь диск в поисках потерянных томов и папок
gui-carve = Также искать файлы по сигнатурам
gui-session-file = Файл сессии
gui-change = Изменить…
gui-scan = Сканировать
gui-stop = Остановить
gui-ok = OK
gui-cancel = Отмена
gui-back = Продолжить
gui-recover-button = { $count ->
        [0] Восстановить
        [one] Восстановить { $count } файл
        [few] Восстановить { $count } файла
       *[many] Восстановить { $count } файлов
    }
gui-removable = съёмный
gui-protected = системный диск · защищён
gui-fact-path = Путь
gui-fact-size = Размер
gui-fact-serial = Серийный номер
gui-fact-folders = папок: { $count }
gui-fact-files = файлов: { $count }
gui-system-disk = С этого диска работает система. Rescyo его только читает, но восстанавливать на него нельзя, а удалённые с него файлы всё время перезаписываются: по возможности восстанавливайте с другого компьютера.
gui-sheet-target = Папка для восстановления
gui-sheet-target-hint = Папка на другом диске. Будет создана при необходимости; ничего в ней не перезаписывается.
gui-sheet-image = Открыть образ диска
gui-sheet-image-hint = Raw, VHD, VHDX, VMDK, qcow2 или E01. Введите полный путь.
gui-sheet-session = Открыть сессию
gui-sheet-session-hint = Файл .rescyo от прежнего сканирования.
gui-sheet-session-file = Файл сессии
gui-sheet-session-hint-new = Куда сохранить найденное.
gui-sheet-session-file-hint = Куда сохранить найденное. Выберите диск, отличный от сканируемого.
gui-sheet-image-out = Файл образа
gui-sheet-image-out-hint = Файл на другом диске, где хватит места на весь диск. Карта рядом с ним позволит продолжить копирование.
gui-scanning = Сканирую { $source }
gui-imaging = Снимаю образ { $source }
gui-scan-image = Сканировать образ
gui-note-bad = Нечитаемые секторы.
gui-note-lost-title = Потерянные папки.
gui-insp-path = Путь
gui-recovering = Восстанавливаю файлов: { $count }
gui-recovered = Восстановлено файлов: { $files } · { $size }
gui-sum-failed = Не удалось записать
gui-search = Поиск среди { $count } файлов…
