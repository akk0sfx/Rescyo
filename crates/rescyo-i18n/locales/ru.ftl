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
