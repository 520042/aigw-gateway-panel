# workbuddy-gateway — x86_64 (linux/amd64) 单阶段镜像
#
# 设计目标：**除基础镜像外，构建过程完全不需要网络**。
#   · 不 COPY 源码、不拉 golang 镜像（二进制在本机交叉编译好，CGO_ENABLED=0 静态）
#   · 不做 apk add：CA 根证书随包提供（cacert.pem），时区库编译时已内嵌（-tags timetzdata）
#   · 不写 `# syntax=` 前端指令 → 不需要额外拉 docker/dockerfile 镜像
#
# 用法：把本目录整个上传到 x86_64 目标机，然后 `docker compose up -d --build`
# 基础镜像换源：改 docker-compose.yml 里的 RUNTIME_IMAGE 即可（见文件内注释）

ARG RUNTIME_IMAGE=docker.m.daocloud.io/library/alpine:3.20
FROM ${RUNTIME_IMAGE}

LABEL org.opencontainers.image.title="workbuddy-gateway" \
      org.opencontainers.image.description="WorkBuddy / CodeBuddy 本地 OpenAI 兼容网关（含 Web 控制台）" \
      org.opencontainers.image.version="1.29.6"

# 1) CA 根证书。Go 在 Linux 上按序读 /etc/ssl/certs/ca-certificates.crt 等路径；
#    上游是 HTTPS 且代码显式 InsecureSkipVerify: false → 没有这份证书会直接 TLS 失败。
COPY cacert.pem /etc/ssl/certs/ca-certificates.crt

# 2) 静态二进制（linux/amd64，go1.24.5，版本 1.29.6）
COPY workbuddy-gateway /usr/local/bin/workbuddy-gateway
RUN chmod 755 /usr/local/bin/workbuddy-gateway

ENV TZ=Asia/Shanghai
WORKDIR /data

EXPOSE 8317
VOLUME ["/data"]

# /health 免鉴权，即使设了控制台密码也能探活
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD wget -q -O /dev/null http://127.0.0.1:8317/health || exit 1

# 故意不写 USER：容器以 root 跑，与线上 192.168.1.8 一致。
# 原因是 /data 是 bind mount，宿主目录由 root 创建 → 用 1000:1000 会写不了 logs/状态文件。
ENTRYPOINT ["/usr/local/bin/workbuddy-gateway"]
CMD ["serve", "-addr", "0.0.0.0", "-port", "8317"]
