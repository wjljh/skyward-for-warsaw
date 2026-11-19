# Skyward for REDMI K90 Ultra (warsaw)

上游 [kirklin/skyward](https://github.com/kirklin/skyward)（GPL-3.0）的本机适配分支，基于上游 c5e09f6。
目标机型：REDMI K90 Ultra（warsaw），HyperOS OS4.0.8.0.XHPCNXM。

## 适配内容

| 问题 | 原因 | 处理 |
|---|---|---|
| 解不开 data 分区 | 厂商 eSE HAL 只在 `init()` 里给安全元件上电，Skyward 先问 `isCardPresent()` 得到"没有" | `skyward-omapi`：先连接再询问 |
| 加上一条仍找不到安全元件 | `ro.product.product.name` 沿用了系统里的 `miproduct_warsaw`，厂商 HAL 走机型专用路径，在 recovery 里无法给 eSE 上电 | 保留系统其余属性，只把该属性设为 `skyward` |
| WLAN 起不来 | 配置里要的 `wlan_mac.bin` 这台机没有 | 删掉该固件项 |
| 息屏后触屏失效 | 背光关闭后面板被 rest，这颗 goodix 在 recovery 里回不来 | 只关背光不 rest 面板，亮屏时重写触摸参数 |
| 厂商 HAL 缺库 | 运行时 APEX 未挂载、`ld.config.txt` 没有 `[odm]` 段 | 用 `[[service]]` 一次脚本解包 APEX、拷贝库、重写 `ld.config.txt`、准备好设备节点 |

## 新增功能

- `[device] unlock_first`：开机停在解密页，data 未解开前只能解密或重启
- 多包安装：选文件即入队，依次安装，逐个给出结果，失败可一键续装
- 刷写分区镜像：先选 boot / init_boot / vendor_boot / recovery / dtbo / vbmeta，再选镜像
- 左上角返回回到所属页面，侧滑返回退一级
- 备份完成显示实际占用与原大小之比
- 关于页显示适配者、修复项，系统版本可点进系统更新

## 构建与刷入

```sh
cargo ndk -t arm64-v8a --platform 30 build --release -p skyward
cargo ndk -t arm64-v8a --platform 30 build --release -p skyward-system-init
./target/release/skyward-pack recovery --stock stock-recovery.img \
  --binary target/aarch64-linux-android/release/skyward \
  --system-init target/system-init/aarch64-linux-android/release/system-init \
  --config devices/warsaw-4.0.8.0.toml \
  --font resources/fonts/MiSansVF.ttf \
  --mono resources/fonts/KirkMono-Regular.ttf \
  --mono-license resources/fonts/KirkMono-OFL.txt \
  --out skyward-warsaw.img

fastboot flash recovery skyward-warsaw.img
fastboot reboot recovery
```

镜像 100 MiB，不含内核，使用 boot 分区的内核启动；只写入 recovery 分区。

## 已知限制

- USB 大容量存储已搁置：data 是 FBE，整盘导出在电脑上是密文，且导出需卸载 /data（曾损坏文件系统）。请用 `adb pull` 或设置里的 MTP
- 触摸驱动不做自动重载：运行中重载会在本机内核留下调用栈且触屏失效，Skyward 只写诊断日志
- MTP 传输进度无法显示：MTP 在手机自身进程中运行

适配：wjljh、DeepSeek Harness
