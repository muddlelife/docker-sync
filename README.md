# Docker Sync

通过网页将镜像同步到自己的阿里云 ACR 仓库。

本仓库用于公开项目源码和发布服务镜像。使用时，请另建一个**私有执行仓库**运行个人同步任务。

## 工作原理

Web 服务保存镜像规则和仓库凭据，通过 GitHub API 触发执行仓库中的 Actions。任务中的仓库凭据会加密传递。

GitHub Actions 使用预先发布的同步工具镜像调用 Skopeo，将源镜像同步至目标仓库；同步时无需编译 Rust。完成后，Web 获取任务结果，展示状态、耗时和日志。镜像数据经 Actions 执行机器传输，不经过 Web 服务。

## 启动

安装 Docker 和 Docker Compose，将 [compose.yml](compose.yml) 保存到一个空目录，在该目录执行：

```sh
docker compose up -d
docker compose exec web /app/docker-sync-web show-setup
```

打开 `http://服务器IP:8000`，输入命令显示的管理员密码。

服务镜像：`ghcr.io/muddlelife/docker-sync:latest`。

公网访问请配置 HTTPS 反向代理，并在同目录的 `.env` 中设置 `COOKIE_SECURE=true`；代理与服务同机时，可同时设置 `BIND_ADDRESS=127.0.0.1`。修改后执行 `docker compose up -d`。

## 连接 GitHub

### 1. 准备执行仓库

1. 在自己的 GitHub 账号下创建仓库，例如 `image-sync`，可见性选择 **Private**。
2. 在控制台“连接设置”下载同步工作流（或保存[工作流模板](workflows/web-sync.yml)），原样上传至私有执行仓库默认分支的 `.github/workflows/web-sync.yml`。
3. 打开执行仓库 **Settings → Actions → General**，在 **Actions permissions** 选择 **Allow all actions and reusable workflows**，点击 **Save**。下方 **Workflow permissions** 保持默认只读即可。
4. 打开仓库 **Actions** 页面，确认能看到 **Web 镜像同步 v2**；如果显示已禁用，点击 **Enable workflow**。

执行分支也需包含同一份最新工作流。[GitHub Actions 设置说明](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/enabling-features-for-your-repository/managing-github-actions-settings-for-a-repository)

同步日志和结果会记录镜像名称及目标地址，请保持执行仓库私有。

### 2. 创建 Token 并授权

1. 打开 GitHub 头像菜单 **Settings → Developer settings → Personal access tokens → Fine-grained tokens → Generate new token**，或[直接打开创建页面](https://github.com/settings/personal-access-tokens/new)。
2. 填写 Token 名称、有效期，**Resource owner** 选择执行仓库所属的账号或组织。
3. **Repository access** 选择 **Only select repositories**，只勾选刚创建的执行仓库。
4. 在 **Permissions → Repository permissions** 添加以下权限：

| 权限 | 级别 | 适用范围 |
| --- | --- | --- |
| Actions | Read and write | 公开、私有仓库均需 |
| Secrets | Read and write | 公开、私有仓库均需 |
| Contents | Read-only | 私有仓库必需 |

5. 点击 **Generate token**，复制生成的 Token。组织要求审批时，先请组织管理员批准。[GitHub Token 创建说明](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens)

### 3. 连接控制台

在“连接设置”填写仓库所有者、仓库名称、执行分支和 Token，点击“保存并初始化”，等待显示“已就绪”。无需手工创建 GitHub Secret。

Token 过期或权限调整后，更新控制台中的 Token，再点击“保存并初始化”。

## 添加目标仓库

先在 ACR 创建命名空间和镜像仓库，再到控制台“目标仓库”点击“添加仓库”：

| 字段 | 填写内容 |
| --- | --- |
| 显示名称 | 如“阿里云主仓库” |
| 仓库地址 | ACR 提供的域名，不带 `https://` 或路径 |
| 命名空间 | ACR 中创建的命名空间 |
| 登录用户名、密码 | ACR 的仓库访问凭据 |

## 添加镜像并同步

进入“镜像规则”，点击“添加规则”：

| 字段 | 示例 |
| --- | --- |
| 规则名称 | Nginx |
| 源镜像 | `nginx:latest` |
| 目标仓库 | 选择已添加的 ACR 配置 |
| 目标镜像名称 | `nginx` |
| 目标标签 | `latest` |
| 同步架构 | 全部架构、AMD64 或 ARM64 |

保存后点击“同步”，或勾选多条规则后点击“同步所选”。分别同步 AMD64、ARM64 时，使用不同标签，如 `latest-amd64`、`latest-arm64`。

## 查看与管理

- **镜像规则**：查看状态和耗时，搜索、筛选、分页；点击“查看记录”打开任务详情。
- **同步任务**：查看执行结果和日志；失败后可点击“重新同步”。
- **仓库镜像**：选择目标仓库，查询远端标签、架构和 digest。自动发现不完整时，填写镜像名称单独查询。
- 列表默认每页 10 条，可切换为 5、20 或 50 条。
- 移除规则会保留远端镜像；删除远端镜像请到 ACR 控制台操作。

## 更新

```sh
docker compose pull
docker compose up -d
```

若同步工作流有更新，从“连接设置”重新下载，原样更新到执行仓库的默认分支和执行分支，再点击“保存并初始化”。

## 修改密码

登录后进入“账户安全”，填写当前密码、新密码和确认密码，点击“修改密码”。新密码使用 12 至 128 个字符。

修改后立即生效，所有已登录设备需要使用新密码重新登录，无需重启服务。

忘记密码时，在部署目录执行：

```sh
docker compose exec web /app/docker-sync-web reset-password
docker compose restart web
```

## 备份数据

```sh
docker compose stop web
docker compose cp web:/data ./backup-data
docker compose start web
```

## 删除服务和数据

以下操作会删除管理员密码、连接设置、仓库配置、镜像规则和任务记录。需要保留时请先备份，ACR 中的远端镜像会保留。

使用本项目提供的 `compose.yml` 时，在部署目录执行：

```sh
docker compose down --volumes
```

再次按“启动”步骤运行会创建全新的管理员密码和配置。
