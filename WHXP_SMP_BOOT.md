# WHPX SMP Boot 方案

## 概述

在 Windows 10 25H2 (build 26200) 上实现 crosvm WHPX 后端的 SMP (--cpus 2) 启动。通过对齐 QEMU WHPX 实现并修复关键平台问题，OVMF 固件可以完成完整启动流程到达 UEFI Shell。

## 文件变更

| 文件 | 变更类型 |
|------|---------|
| `hypervisor/src/whpx/vm.rs` | 分区属性顺序对齐、State2 API、APIC 模式 |
| `hypervisor/src/whpx/vcpu.rs` | per-vCPU APIC ID、StartupSuspend 清除、INIT/SIPI handler、MSR 拦截 |
| `hypervisor/src/whpx.rs` | 导入整理 |
| `hypervisor/src/whpx/whpx_sys/bindings.rs` | State2 API 声明 |
| `devices/src/irqchip/whpx.rs` | IrqChipCap::X2Apic 启用 |
| `src/sys/windows/run_vcpu.rs` | vCPU 初始化流程 (set_apic_id, push_reset_state) |
| `x86_64/src/lib.rs` | **AcpiPmTimer** (port 0x608) |

## 核心修改详解

### 1. 分区属性顺序 (vm.rs)

**问题**: WHvSetupPartition 必须在所有属性设置之后调用。QEMU 将所有 WHvSetPartitionProperty 调用放在 WHvSetupPartition 之前。

**修复**: 调整顺序为:
1. ProcessorCount
2. LocalApicEmulationMode = X2Apic
3. ProcessorFeaturesBanks (get capability + set)
4. SyntheticProcessorFeaturesBanks
5. CpuidResultList + CpuidExitList
6. ExtendedVmExits
7. WHvSetupPartition (最后)

**关键细节**: LocalApicEmulationMode 属性值大小使用 `sizeof(mode)` = 4 字节 (enum)，而非 `sizeof(WHV_PARTITION_PROPERTY)` = full union。这与 QEMU 完全一致。

### 2. State2 API 升级 (vm.rs + vcpu.rs)

**问题**: crosvm 使用已弃用的 `WHvSetVirtualProcessorInterruptControllerState` (State1 API)。QEMU 使用 `WHvSetVirtualProcessorInterruptControllerState2` (State2 API)，且 kernel-irqchip=on 模式明确要求 State2。

**修复**: 
- get_vcpu_lapic_state / set_vcpu_lapic_state 使用 State2 API
- set_apic_id() 通过 State2 API 设置 per-vCPU APIC ID
- 正确的 LAPIC 寄存器索引: QEMU whpx_lapic_state 中 fields[N].data 在 offset N*16，对应 crosvm WhpxLapicState 的 regs[N*4]

### 3. Per-vCPU APIC ID (vcpu.rs)

**问题**: 没有唯一 APIC ID 时，所有 vCPU 的 x2APIC MSR 0x802 返回 0。AP 误认为自己是 BSP，进入错误的初始化代码路径。

**修复**: `set_apic_id()` 通过 State2 API 设置:
- vcpu=0: APIC ID = 0 (BSP)
- vcpu=1: APIC ID = 1 (AP)
- 同时设置 APIC Version (0x50014) 和 SVR (APIC enabled)

### 4. vCPU 复位状态推送 (vcpu.rs + run_vcpu.rs)

**问题**: QEMU 的 `whpx_cpu_synchronize_post_reset` 在首次 WHvRunVirtualProcessor 之前推送完整 vCPU 状态。crosvm 未推送。

**修复**: `qemu_push_reset_state()` 在首次 run 之前推送 62 个寄存器 (GPRs + SREGs + FPU + XCR0 + APIC_BASE)，对齐 QEMU 的 WHPX_LEVEL_RESET_STATE。

### 5. StartupSuspend 清除 (vcpu.rs)

**问题**: WHPX X2Apic 模式将 AP vCPU 置于 StartupSuspend (bit 0) 状态 (Activity=0x1)。此状态下 WHvRunVirtualProcessor 不执行指令。QEMU 的 `whpx_vcpu_kick_out_of_hlt` 仅清除 HaltSuspend (bit 1)，在新版 Windows 上不足以唤醒 AP。

**修复**: `kick_out_of_halt()` 清除所有 suspend 位 (StartupSuspend bit 0 + HaltSuspend bit 1 + IdleSuspend bit 2)。

### 6. ACPI PM Timer (x86_64/lib.rs) — 关键突破

**问题**: OVMF PEI 阶段的 MicroSecondDelay 使用 `AcpiTimerLib` 读 ACPI PM Timer (port 0x608)。crosvm 未模拟此端口，read 返回 0 (不变)。导致 BSP 在 CpuMpPei 的 WakeUpAp → MicroSecondDelay 中死循环。

**修复**: 实现最小化的 AcpiPmTimer BusDevice，在 port 0x608 提供递增的 32-bit 计数器 (~3.58MHz ACPI PM timer 频率)。

**为什么 --cpus 1 不受影响**: CpuMpPei 检测到只有 1 个 CPU 时跳过 WakeUpAp，不调用 MicroSecondDelay。

### 7. INIT/SIPI Handler (vcpu.rs)

**问题**: 原 handler 未处理 DEST_ALLBUT (destination shorthand 3)，OVMF 最常见的 INIT 广播模式。

**修复**: 重构 handler 覆盖所有 destination shorthand:
- 0: Physical destination (使用 x2APIC destination field)
- 1: Self (自检)
- 2: AllIncludingSelf (广播)
- 3: AllExcludingSelf (排除自身广播) — OVMF 使用的模式

添加 WHvRequestInterrupt 调用的错误日志。

### 8. MSR 0x830 拦截 (vcpu.rs) — 备选路径

对齐 QEMU kernel-irqchip=off 方案。在 handle_msr_write 中拦截 x2APIC ICR (MSR 0x830) 写入，手动投递 INIT/SIPI。

当前 X2Apic 模式下 WHPX 内部处理 MSR 0x830，此拦截器未被调用。作为备选方案保留，用于未来可能的 kernel-irqchip=off 模式或 WHPX 行为变化。

### 9. x2APIC MSR 处理 (vcpu.rs)

handle_msr_read/write 中添加 x2APIC MSR (0x800-0x8FF) 的基础处理:
- 0x802: 返回 per-vCPU x2APIC ID
- 0x803: 返回 APIC version 0x50014
- 其他: RAZ/WI (读返回 0，写静默忽略)

### 10. CpuidExitList 添加 Leaf 0xB (vm.rs)

x2APIC 拓扑枚举需要 CPUID leaf 0xB (Extended Topology)。在 CpuidExitList 中包含此 leaf，确保 crosvm 的 adjust_cpuid 设置 per-vCPU x2APIC ID (EDX = vcpu_id)。

## 启动流程

```
1. 分区创建
   ├── ProcessorCount = 2
   ├── LocalApicEmulationMode = X2Apic
   ├── ProcessorFeaturesBanks (get + set)
   ├── SyntheticProcessorFeaturesBanks (set)
   ├── CpuidResultList (Hyper-V leaves)
   ├── CpuidExitList (0x1, 0x4, 0xB, 0x1F, 0x15)
   ├── ExtendedVmExits (X64CpuidExit + X64MsrExit)
   └── WHvSetupPartition ← 最后

2. vCPU 初始化
   ├── vcpu=0: set_apic_id(0) + qemu_push_reset_state()
   └── vcpu=1: set_apic_id(1) + qemu_push_reset_state()

3. BSP (vcpu=0) 启动
   ├── OVMF SEC → PEI
   ├── CpuMpPei: 发送 INIT/SIPI (WHPX 内部处理)
   ├── MicroSecondDelay(10) ← AcpiPmTimer 正常工作
   ├── TimedWaitForApFinish ← QEMU 对齐后 BSP 继续
   ├── DxeIpl → DXE Core → BDS
   ├── VirtioBlkDxe: 发现 Mass Storage
   └── Shell.efi 加载

4. AP (vcpu=1) 状态
   ├── WHPX 置于 StartupSuspend (Activity=0x1)
   ├── WHPX 不向 dormant AP 投递 INIT/SIPI
   ├── AP 可手动唤醒: apply_sipi_vector + kick_out_of_halt
   └── AP 可执行指令 (CPUID exits 验证)
```

## 已知限制

1. **WHPX 不投递 INIT/SIPI 到 dormant AP**: vcpu=1 需要手动 apply_sipi_vector + kick_out_of_halt 才能启动。AP 启动后进入错误代码路径 (CPUID loop @ 0xBFF5C2D5)，因为未通过标准 MP Startup Stub。

2. **AP 未完整加入 MP 同步**: BSP 通过 TimedWaitForApFinish 是因为 WHPX 内部完成了 INIT/SIPI 处理且 APIC timer 正常工作。但 AP 未通过 ExchangeInfo → MP Startup Stub 路径回签。

3. **AP 完整启动需要**: 正确的 ExchangeInfo 初始化 + non-16-bit 启动向量 (0xBFF35000) + flat protected mode 设置。这些在 WHPX 正确投递 INIT/SIPI 时会自动由 WHPX 完成。

## QEMU 对齐项对照

| QEMU | crosvm | 状态 |
|------|--------|------|
| whpx_accel_init 属性顺序 | WHvSetupPartition 前设置所有属性 | ✅ |
| sizeof(mode) = 4 | 相同 | ✅ |
| WHvSetVirtualProcessorInterruptControllerState2 | 相同 | ✅ |
| whpx_cpu_synchronize_post_reset | qemu_push_reset_state() | ✅ |
| whpx_vcpu_kick_out_of_hlt | kick_out_of_halt() | ✅ (增强: 清除所有 suspend bits) |
| whpx_apic_put (APIC ID per-vCPU) | set_apic_id() | ✅ |
| ApicRemoteReadSupport | 未设置 (QEMU 不设置) | ✅ |
| ProcessorFeaturesBanks + SyntheticProcessorFeatures | 对齐 | ✅ |
| ExceptionExitBitmap = 0 | 未设置 (QEMU 显式设置) | ⚠️ |
| X64ApicInitSipiExitTrap | 未启用 (导致 BSP deadlock) | ⚠️ |
| AP vCPU 启动 (BQL 序列化) | 同时启动 (barrier) | 差异 |

## 调试历程关键发现

1. **ActivityState bit 0 = StartupSuspend**: WHPX 在新版 Windows 上将 AP 置于 StartupSuspend 而非 HaltSuspend。QEMU 的 kick_out_of_hlt 仅清除 bit 1。

2. **ACPI PM Timer**: BSP MicroSecondDelay 死锁的真正原因。port 0x608 返回 0 → delay 循环永不退出。

3. **GPA 缓存**: WHPX 缓存 guest 物理内存。VMM 侧写入 + WHvCancelRunVirtualProcessor 不刷新缓存。

4. **CpuMpData 扫描**: 0xBC~0xBD 范围扫描找到 CpuCount=2 定位 CpuMpData。仅用于诊断，已从最终方案中移除。

5. **FinishedCount 注入**: 向 GuestMemory 写入 FinishedCount=1 绕过 TimedWaitForApFinish。证明 BSP 等待点，最终 ACPI PM Timer 修复后不再需要。
