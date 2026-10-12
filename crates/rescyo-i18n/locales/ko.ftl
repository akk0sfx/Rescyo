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
recover-intact = 파일 { $count }개를 형식에 맞게 검증했으며 손상이 없습니다.
recover-damaged = 파일 { $count }개가 형식에 맞게 읽히지 않습니다. 보고서를 확인하십시오.
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
error-image-unsupported = { $source }을(를) 열 수 없습니다: { $reason }. raw 이미지로 변환한 뒤(예: qemu-img) 여세요.
error-parent-missing = { $source }은(는) 차등 이미지인데 부모 이미지를 찾을 수 없습니다(시도한 경로: { $tried }). 부모 이미지를 같은 위치에 두세요.
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

## Imaging
image-start = { $source }의 이미지를 { $image }에 만드는 중(맵: { $map })…
container-opened = { $format } 이미지: 가상 디스크 { $size }, 파일 { $files }개.
container-note = 이미지 참고: { $note }
phase-image-copy = 복사
phase-image-trim = 불량 영역 다듬기
phase-image-scrape = 불량 영역 섹터별 읽기
phase-image-retry = 불량 섹터 재시도
image-rescued = 복구됨
image-bad-short = 불량
image-done = 이미지 완료: { $size } 중 { $rescued } 복구.
image-bad = 불량 영역 { $count }개({ $size })를 읽지 못했습니다. 이미지에서는 0으로 채워지고 맵에 표시됩니다. 이미지를 스캔할 때 --map을 지정하세요.
error-image-exists = { $path }이(가) 이미 있으며 맵이 없습니다. Rescyo는 이미지를 덮어쓰지 않습니다. 다른 파일 이름을 고르거나 해당 --map을 지정하세요.
error-map-invalid = 맵 { $path }을(를) 읽을 수 없습니다: { $reason }
error-map-mismatch = 맵의 디스크 크기는 { $map }이지만 소스는 { $source }입니다.
error-image-cancelled = 중지했습니다. 맵이 저장되었습니다. 같은 명령을 다시 실행하면 계속합니다.
hint-image-resume = 맵이 저장되었습니다. 디스크를 다시 연결하고 같은 명령을 다시 실행하면 계속합니다.

## Partition search
partitions-start = { $source }에서 부트 섹터를 찾는 중…
partitions-table = 파티션 테이블: { $table }
partitions-none = 볼륨을 찾지 못했습니다.
partition-listed = #{ $number } ({ $offset }): { $fs }{ $label }, { $size }, 테이블에 있음 ({ $boot })
partition-lost = 손실된 파티션 ({ $offset }, 바이트 { $bytes }): { $fs }{ $label }, { $size }, 테이블에 없음 ({ $boot })
partition-conflicting = 볼륨 ({ $offset }, 바이트 { $bytes }): { $fs }{ $label }, { $size }, 테이블 항목과 겹침 ({ $boot })
partition-nested = 다른 볼륨 안의 볼륨 ({ $offset }): { $fs }{ $label }, { $size } ({ $boot })
partition-empty = #{ $number } ({ $offset }, { $size }): 시작 위치에서 볼륨을 찾지 못함
partition-boot-both = 부트 섹터와 백업
partition-boot-primary = 부트 섹터
partition-boot-backup = 백업 부트 섹터만
partition-boot-none = 부트 섹터 없음
partition-unpartitioned = 파티션 테이블 없음: 디스크 전체가 하나의 볼륨, { $fs }{ $label }, { $size } ({ $boot })
scan-carved = 볼륨 { $id }: 시그니처로 파일 { $files }개 발견(유형별 폴더에 정리, 복구할 때 내용을 검사)

## Terminal interface
tui-step-source = 소스
tui-step-scan = 스캔
tui-step-browse = 탐색
tui-step-recover = 복구
tui-search = 파일 { $count }개에서 검색…(/)
tui-fact-cluster = 클러스터
tui-fact-data = 데이터 시작
tui-fact-folders = 폴더
tui-fact-files = 파일
tui-col-name = 이름
tui-col-size = 크기
tui-col-modified = 수정 시간
tui-col-assembly = 조립 방식
tui-col-integrity = 무결성
tui-intact = 정상
tui-partial = 일부
tui-damaged = 손상
tui-unverified = 확인 안 됨
tui-empty = 파일이 없습니다.
tui-insp-runs = 조각
tui-insp-state = 상태
tui-insp-kind = 종류
tui-deleted = 삭제됨
tui-live = 있음
tui-stream = 명명된 스트림
tui-note-lost = 부모를 잃은 폴더 { $count }개는 _LOST_FOLDERS 아래에 있습니다.
tui-scanning = { $source } 스캔 중
tui-recover-to = 복구 위치
tui-no-target = T를 눌러 다른 디스크의 폴더를 선택하세요
tui-preflight-ok = 사전 점검: 다른 장치 · 여유 공간 { $free } · 이름 정상
tui-preflight-problems = 문제 { $count }개: { $first }
tui-recover-button = R  파일 { $count }개 복구
tui-input-target = 복구할 폴더
tui-input-hint = 다른 디스크에 있는 폴더. Enter로 확인, Esc로 취소.
tui-running = 복구 중… { $done } / { $total } (Esc로 중지)
tui-cancelled = 중지했습니다. 이미 기록된 파일은 완전하며 보고서에 나와 있습니다.
tui-source-changed = 소스가 이 세션과 더 이상 일치하지 않습니다. 확실하다면 명령줄에서 --force-rebind를 사용하세요.
tui-help-title = 키
tui-help =
    ↑ ↓ 또는 j k      이동          Tab        폴더와 파일 전환
    → ← 또는 Enter    열기, 닫기    Space      파일 표시    a   여기 모두 표시
    /                 검색          d          삭제된 파일 표시/숨기기
    s                 명명된 스트림  t         대상 폴더 선택
    r                 복구          q 또는 Esc 종료
tui-keys =  Tab 패널 · Space 표시 · / 검색 · d 삭제됨 · t 대상 · r 복구 · ? 도움말 · q 종료
