# Conduit API

> 感谢Linxu.do支持,学AI,上L站!!!以及任何bug请让Codex小姐背锅

[English](README.md) | [简体中文](README_CN.md)

[![发布检查](https://github.com/404F0X/Conduit-API/actions/workflows/release-gates.yml/badge.svg)](https://github.com/404F0X/Conduit-API/actions/workflows/release-gates.yml)
[![CodeQL](https://github.com/404F0X/Conduit-API/actions/workflows/codeql.yml/badge.svg)](https://github.com/404F0X/Conduit-API/actions/workflows/codeql.yml)

Conduit API 是一个可自行部署的 AI 网关。你可以在同一个网页控制台中接入不同的上游服务、配置模型路由、管理访问权限，并查看用量与计费信息。

> [!WARNING]
> Conduit API 目前仍处于 Alpha 阶段。升级前请备份 PostgreSQL，并先在测试环境验证新版本，再接入生产流量。

## 主要功能

- 兼容 OpenAI、Anthropic、Gemini、Jina、豆包及 AI SDK 等协议。
- 按上游健康状态路由请求，并支持重试、并发限制和提示词缓存亲和。
- 自动发现上游模型，管理模型映射，并审核价格更新。
- 支持项目、细粒度 API Key、用量统计、钱包、订阅和可重复使用的兑换码。
- 支持 OIDC 登录、审计记录、加密配置备份和可选的 Redis 集成。
- 提供支持英文与简体中文的 React 管理控制台。

## 快速开始

开始前请安装 Git、Docker 和 Docker Compose。默认部署只监听
`127.0.0.1:8090`，数据保存在持久化的 PostgreSQL 数据卷中。

在 Bash 等兼容的终端中执行：

```sh
git clone https://github.com/404F0X/Conduit-API.git
cd Conduit-API
export CONDUIT_POSTGRES_PASSWORD='replace-with-a-long-random-value'
docker compose config --quiet
docker compose up --build -d
curl -fsS http://127.0.0.1:8090/health
```

由于 Compose 会将数据库密码放入 PostgreSQL 连接地址，请只使用字母、数字、
`-`、`.`、`_` 或 `~`。

在 Windows PowerShell 中执行：

```powershell
git clone https://github.com/404F0X/Conduit-API.git
Set-Location Conduit-API
$env:CONDUIT_POSTGRES_PASSWORD = 'replace-with-a-long-random-value'
docker compose config --quiet
docker compose up --build -d
Invoke-WebRequest http://127.0.0.1:8090/health | Out-Null
```

随后打开 <http://127.0.0.1:8090>。系统没有默认管理员密码，首次访问时需要创建站点所有者账号，并选择实际记账货币和站内积分的显示名称。请在初始化时认真核对这些信息。

## 开始使用

1. 在管理控制台中添加上游服务并填写对应凭据。
2. 自动发现或手动创建模型，然后设置模型映射与路由。
3. 创建项目，并为项目签发 API Key。
4. 将兼容 OpenAI 的客户端地址设为 `http://127.0.0.1:8090/v1`，并将项目 API Key 作为 Bearer Token 使用。

普通用户可在**钱包**页面查看余额和兑换积分码；管理员可在**计费**页面创建、限制使用次数或撤销兑换码。

## 下载与部署

预编译的 Linux、Windows 程序包和容器镜像可从
[GitHub Releases](https://github.com/404F0X/Conduit-API/releases) 页面获取。运行下载的程序前，请使用随附的 `SHA256SUMS` 文件校验完整性。

如需将 Conduit API 暴露到公网，请先阅读
[生产部署指南](https://github.com/404F0X/Conduit-API/blob/main/docs/production-deployment.md)。请启用 TLS，正确设置公开地址和浏览器允许来源，将密码与 Token 保存在仓库外，并定期验证 PostgreSQL 备份能否恢复。

## 帮助与项目信息

- [生产部署](https://github.com/404F0X/Conduit-API/blob/main/docs/production-deployment.md)
- [安全策略](https://github.com/404F0X/Conduit-API/blob/main/SECURITY.md)
- [开发路线](https://github.com/404F0X/Conduit-API/blob/main/ROADMAP.md)
- [参与贡献](https://github.com/404F0X/Conduit-API/blob/main/CONTRIBUTING.md)
- [发布与构建细节](RELEASE_GATES.md)
- [重新构建与重链接](RELINKING.md)

如遇到可复现的问题，请通过
[GitHub Issues](https://github.com/404F0X/Conduit-API/issues) 反馈。请勿在 Issue 中提交密码、API Key、上游响应、提示词或其他秘密信息。

## 许可证

仓库大部分内容采用 Apache-2.0 许可证；[LICENSE](LICENSE) 中列出的协议核心 crate 采用 LGPL-3.0-only 许可证。必须保留的署名信息位于 [NOTICE](NOTICE) 和 [frontend/NOTICE](frontend/NOTICE)。
