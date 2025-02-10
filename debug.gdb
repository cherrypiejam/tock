set disassemble-next-line on
set confirm off
add-symbol-file /home/cherrypie/hack/larps/bridge/tock/target/riscv32imac-unknown-none-elf/release/qemu_rv32_virt_smp.elf
target remote tcp::1234
set arch riscv
layout regs
thread 1
set scheduler-locking off