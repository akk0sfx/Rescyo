# Rescyo 명령줄 문자열. 기계용 출력(JSON)은 현지화하지 않습니다. 원어민 검수 대기.

app-about = 디스크에 절대 쓰지 않는 무료 오픈 소스 데이터 복구 도구.

disks-none = 디스크를 찾을 수 없습니다. 디스크를 읽으려면 관리자 또는 root 권한이 필요할 수 있습니다.
disks-size-unknown = 크기 알 수 없음(관리자 또는 root 권한 필요)
disks-removable = 이동식

scan-start = { $source } 스캔 중…
phase-probe = 볼륨을 찾는 중
phase-deep-scan = 원본 전체를 읽는 중
phase-walk = 폴더를 재구성하는 중
phase-lost-folders = 손실된 폴더를 배치하는 중
scan-volume = 볼륨 { $id }: { $fs }, 클러스터 { $cluster }, 파일 { $files }개, 폴더 { $dirs }개, 삭제됨 { $deleted }개, 손실 { $lost }개
scan-volume-skipped = 볼륨 { $id }: { $offset }의 { $fs }(점수 { $score }), 탐색하지 않음
scan-no-volumes = 볼륨을 찾지 못했습니다. 정밀 스캔(--deep)을 시도해 보십시오.
scan-bad-sectors = 읽을 수 없는 섹터 { $count }개를 건너뛰고 기록했습니다.
scan-done = 세션을 저장했습니다: { $session }

import-summary = DMDE 볼륨 { $index }: 클러스터 { $cluster }, 데이터 시작 { $offset }, 로그상 폴더 { $folders }개. 디렉터리 시작점 { $seeds }개를 사용했고 클러스터 격자를 벗어난 { $offgrid }개는 제외했습니다.

ls-total = 항목 { $count }개

recover-plan = 파일 { $files }개, { $size }, 대상: { $target }
recover-done = 파일 { $files }개({ $size })를 복구했습니다. 보고서: { $report }
recover-partial = 파일 { $count }개에 0으로 채운 읽을 수 없는 부분이 있습니다. 보고서를 확인하십시오.
recover-guessed = 파일 { $count }개는 할당 정보 없이 연속된 것으로 가정해 읽었습니다. 사용하기 전에 확인하십시오.
recover-resumed = 파일 { $count }개는 이전 실행에서 이미 대상에 있었고 검증되었습니다.
recover-failed = 파일 { $count }개를 쓰지 못했습니다. 보고서를 확인하십시오.

info-source = 원본: { $path }
info-source-unverified = 세션이 지문이 일치하지 않는 원본에 연결되었습니다. 결과가 검증되지 않았습니다.
info-volumes = 볼륨: { $count }
info-entries = 항목: { $count }

problem-target-on-source-device = 대상이 원본과 같은 물리 디스크에 있습니다({ $devices }). 다른 디스크의 폴더를 선택하십시오.
problem-unverified-target = 대상이 어느 디스크에 있는지 알 수 없습니다({ $reason }). 다른 폴더를 선택하거나, 다른 디스크임이 확실하면 --allow-unverified-target을 사용하십시오.
problem-source-devices-unknown = 원본이 어느 디스크에 있는지 알 수 없습니다({ $reason }).
problem-insufficient-space = 공간이 부족합니다: 필요 { $needed }, 여유 { $free }.
problem-file-too-large = { $path }의 크기는 { $size }로 대상 파일 시스템의 한도({ $limit })를 넘습니다. exFAT 또는 NTFS 대상을 사용하십시오.
problem-nothing-selected = 선택 조건에 맞는 항목이 없습니다.

error-source-open = { $source }을(를) 열 수 없습니다: { $reason }
error-source-mismatch = { $source }은(는) 이 세션을 만든 원본이 아닙니다({ $reason }). 확실한 경우에만 --force-rebind를 사용하십시오.
error-session-exists = { $path }이(가) 이미 있습니다. Rescyo는 세션을 덮어쓰지 않습니다. 다른 파일 이름을 지정하십시오.
error-session-invalid = { $path }은(는) Rescyo 세션이 아닙니다: { $reason }
error-session-on-source = 세션 파일이 원본 디스크에 저장됩니다({ $devices }). 다른 디스크에 저장하십시오.
error-degraded = 원본 상태가 나빠지고 있습니다: 읽을 수 없는 섹터 { $count }개. 즉시 중단하고 먼저 이미지를 만든 뒤(예: GNU ddrescue) 이미지를 스캔하십시오. 그래도 계속하려면 --continue-on-errors를 사용하십시오.
error-gone = 원본 연결이 끊어졌습니다: { $reason }
error-dmde-parse = DMDE 로그를 읽을 수 없습니다: { $reason }
error-dmde-geometry = DMDE 로그는 다른 디스크({ $log })를 설명하며 원본은 { $source }입니다. 확실한 경우에만 --force를 사용하십시오.
error-internal = 내부 오류: { $reason }
error-cancelled = 중지했습니다. 지금까지 찾은 내용은 세션에 저장되어 있습니다.
error-preflight = 아무것도 쓰지 않았습니다. 먼저 다음 문제를 해결하십시오:

hint-admin = 디스크를 읽으려면 Windows에서는 관리자 권한, Linux와 macOS에서는 sudo가 필요합니다.
