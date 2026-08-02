# JwcCrawler 运行测试 Makefile
# 本仓库目录为只读挂载，编译产物统一输出到 CARGO_TARGET_DIR 指向的可写目录（默认 /tmp）

CARGO ?= cargo
TARGET_DIR ?= /tmp/jwc-crawler-target
export CARGO_TARGET_DIR := $(TARGET_DIR)

.PHONY: help test build sidecar fmt clean

help:
	@echo "可用目标："
	@echo "  make test     编译并运行全部测试（hardening / spool_worker / worker_protocol）"
	@echo "  make build    编译库与全部二进制"
	@echo "  make sidecar  仅构建 nlr_sidecar 二进制（spool worker）"
	@echo "  make fmt      运行 rustfmt 格式检查"
	@echo "  make clean    删除编译产物目录（$(TARGET_DIR)）"
	@echo "  make TARGET_DIR=<目录> ...  覆盖编译产物目录"

test:
	$(CARGO) test --all-targets

build:
	$(CARGO) build

sidecar:
	$(CARGO) build --bin nlr_sidecar

fmt:
	$(CARGO) fmt --check

clean:
	rm -rf $(TARGET_DIR)
