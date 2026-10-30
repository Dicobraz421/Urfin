# Общая очередь устройств: DeviceGate в core

(владелец, 2026-10-30) Очередь «задач на устройство» (`[ops.device_jobs]`: HDD 1, SSD 3, сеть 2, USB 1) — одна на процесс и общая для копирования, удаления и скана анализа места. `DeviceGate`, `DeviceLease`, `JobCtl`, `Slot`, `DeviceJobs` переезжают из urfin-ops в `urfin_core::device`; экземпляр — у `Runtime` (`Runtime::devices`, `Core::devices`). Ключ устройства и лимит по носителю — `urfin_vfs::volume::{device_key, jobs_for}`.

Причина: скан HDD параллельно с копированием на тот же HDD замедляет обоих. Класс B (контракт core).
