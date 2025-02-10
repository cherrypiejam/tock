#! /usr/bin/env nix-shell
#! nix-shell -i bash -p bash

make
rm flash.bin
tockloader flash --flash-file flash.bin -a 0x80000000 --board qemu_rv32_virt /home/cherrypie/hack/bridge/tock/target/riscv32imac-unknown-none-elf/release/qemu_rv32_virt_smp.bin
tockloader install --flash-file flash.bin --board qemu_rv32_virt /home/cherrypie/hack/bridge/libtock-c/examples/rot13_client/build/rot13_client.tab
tockloader install --flash-file flash.bin --board qemu_rv32_virt /home/cherrypie/hack/bridge/libtock-c/examples/rot13_service/build/org.tockos.examples.rot13.tab
