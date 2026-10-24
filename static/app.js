"use strict";
// 浏览器只访问本站 API，不保存或直接使用云平台与 GitHub 凭据。
const $ = (id) => document.getElementById(id);
const page = document.body.dataset.page;
let csrf = "", registries = [], rules = [], inventoryData = null, inventoryRequest = 0;
const selectedRules = new Set();
const paging = Object.fromEntries(["rules","registries","images","tasks","items"].map(key=>[key,{page:1,size:10,total:0}]));
let tasksRequest=0, taskDetailItems=[];
function resetPage(key) { paging[key].page=1; }
function pageRows(items,key) {
  const state=paging[key];state.total=items.length;
  state.page=Math.max(1,Math.min(state.page,Math.max(1,Math.ceil(items.length/state.size))));
  return items.slice((state.page-1)*state.size,state.page*state.size);
}
function renderPager(key,total) {
  const state=paging[key],pages=Math.max(1,Math.ceil(total/state.size));state.total=total;state.page=Math.max(1,Math.min(state.page,pages));
  const host=$(key+"-pagination");if(!host)return;
  const signature=JSON.stringify([total,state.page,state.size]);
  if(host.dataset.pagination===signature)return;
  const active=document.activeElement;
  const focus=active&&host.contains(active)?{size:active.dataset.pageSize,direction:active.dataset.pageDirection,number:active.dataset.pageNumber}:null;
  const button=(number,label,current=false,direction="")=>'<button type="button" data-page-key="'+key+'" data-page-number="'+number+'"'+(direction?' data-page-direction="'+direction+'"':'')+(current?' class="current" aria-current="page"':'')+(number<1||number>pages?' disabled':'')+'>'+label+'</button>';
  let numbers=""; const visible=new Set([1,pages,state.page-1,state.page,state.page+1]);let previous=0;
  for(const number of [...visible].filter(n=>n>=1&&n<=pages).sort((a,b)=>a-b)) {if(number>previous+1)numbers+='<span class="page-ellipsis">…</span>';numbers+=button(number,number,number===state.page);previous=number;}
  host.innerHTML='<span class="pagination-total">'+(key==="images"?'本次查询共 ':'共 ')+total+' 条</span><div class="pagination-controls"><label>每页<select aria-label="每页条数" data-page-size="'+key+'">'+[5,10,20,50].map(size=>'<option value="'+size+'"'+(size===state.size?' selected':'')+'>'+size+' 条</option>').join("")+'</select></label>'+button(state.page-1,'上一页',false,'previous')+numbers+button(state.page+1,'下一页',false,'next')+'</div>';
  host.dataset.pagination=signature;
  if(focus) {
    const selector=focus.size?'select[data-page-size]':focus.direction?'[data-page-direction="'+focus.direction+'"]':'[data-page-number="'+focus.number+'"]';
    let target=host.querySelector(selector);if(!target||target.disabled)target=host.querySelector('[aria-current="page"]');target?.focus();
  }
}
async function renderPage(key) {
  if(key==="rules")renderRules();else if(key==="registries")renderRegistries();else if(key==="images")filterInventory();else if(key==="items")renderTaskItems();else if(key==="tasks")await taskList();
}
async function taskList() {
  const state=paging.tasks,request=++tasksRequest;
  const data=await api('/api/tasks/page?number='+state.page+'&size='+state.size);
  if(request!==tasksRequest)return;
  state.page=data.page;state.size=data.page_size;$("task-list").innerHTML=taskTable(data.items);renderPager("tasks",data.total);
}
const esc = (v) => String(v ?? "").replace(/[&<>"']/g, c => ({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));
const labels = {never:"未同步",unchanged:"内容未变化",pending:"等待派发",dispatching:"正在派发",dispatch_unknown:"正在核实派发",dispatch_failed:"派发失败",queued:"GitHub 排队中",running:"正在同步",result_pending:"正在获取结果",succeeded:"同步成功",failed:"同步失败",cancelled:"已取消",needs_review:"需要人工核实"};
const terminal = (s) => ["succeeded","failed","cancelled","dispatch_failed"].includes(s);
const date = (n) => n == null ? "—" : new Date(n * 1000).toLocaleString("zh-CN", {hour12:false});
const platform = (p) => ({all:"全部架构","linux/amd64":"AMD64","linux/arm64":"ARM64"}[p] || p);
function duration(ms) {
  if (!Number.isFinite(ms) || ms < 0) return "—";
  if (ms < 1000) return Math.floor(ms) + " 毫秒";
  const seconds = Math.floor(ms / 1000), minutes = Math.floor(seconds / 60);
  if (minutes === 0) return seconds + " 秒";
  if (minutes < 60) return minutes + " 分 " + seconds % 60 + " 秒";
  return Math.floor(minutes / 60) + " 小时 " + minutes % 60 + " 分 " + seconds % 60 + " 秒";
}
function pill(s) {
  const cls = ["succeeded","unchanged"].includes(s) ? "success" : s === "never" || s === "cancelled" ? "neutral" : ["failed","dispatch_failed","needs_review"].includes(s) ? "error" : s === "running" ? "running" : "pending";
  return '<span class="pill ' + cls + '">' + esc(labels[s] || s) + '</span>';
}
function notice(message, type = "success") {
  const local=type==="error"?document.querySelector?.("dialog[open] [data-editor-notice]"):null;
  const box = local || $("notice"); box.textContent = message; box.className = "notice " + type; box.hidden = false;
}
async function api(url, method = "GET", data) {
  const response = await fetch(url, {method, credentials:"same-origin", headers:{"Content-Type":"application/json", ...(method !== "GET" ? {"X-CSRF-Token":csrf} : {})}, ...(data === undefined ? {} : {body:JSON.stringify(data)})});
  let result; try { result = await response.json(); } catch (_) { throw new Error("服务器返回了无法读取的响应，请稍后重试"); }
  if (!response.ok) {
    if (response.status === 401 && page !== "login") location.assign("/login");
    throw new Error(result.message || "操作失败，请检查输入后重试");
  }
  return result;
}
function action(fn) { return async (event) => {
  if (event?.currentTarget !== document) event?.preventDefault(); const button = event?.submitter || (event?.currentTarget?.tagName === "BUTTON" ? event.currentTarget : null);
  if (button) button.disabled = true;
  try { await fn(event); } catch (e) { notice(e.message, "error"); }
  finally { if (button) button.disabled = false; }
}; }
function formData(form) { const data = Object.fromEntries(new FormData(form)); if (form.elements.enabled) data.enabled = form.elements.enabled.checked; delete data.id; return data; }
function validatePasswordChange(data) {
  const length = Array.from(data.new_password).length;
  if (length < 12 || length > 128) throw new Error("新密码需为 12 至 128 个字符");
  if (data.new_password !== data.confirm_password) throw new Error("两次输入的新密码不一致");
}
function openEditor(id) { const editor=$(id);const local=editor.querySelector("[data-editor-notice]");if(local)local.hidden=true;editor.hidden=false;if(!editor.open)editor.showModal();editor.querySelector("input:not([type=hidden]),select")?.focus(); }
function closeEditor(id) { const editor=$(id);editor.close();editor.hidden=true; }
function empty(title, message) { return '<div class="empty"><strong>' + esc(title) + '</strong>' + esc(message) + '</div>'; }
function fill(form, data) { for (const [key,value] of Object.entries(data)) { const field = form.elements[key]; if (!field || field.type === "password") continue; if (field.type === "checkbox") field.checked = !!value; else field.value = value ?? ""; } }
function taskTable(tasks, compact=false) {
  if (!tasks.length) return empty("还没有同步任务", "添加镜像规则后，点击同步即可创建第一个任务。");
  if(compact) return '<table><thead><tr><th>任务 / 源镜像</th><th>状态</th><th>耗时</th><th></th></tr></thead><tbody>'+tasks.map(t=>'<tr><td><a class="mono" href="/tasks/'+esc(t.id)+'">'+esc(t.id.slice(0,8))+'</a><small>'+esc(t.items[0]?.source||"")+'</small></td><td>'+pill(t.status)+'</td><td>'+duration(t.elapsed_ms)+'</td><td><a href="/tasks/'+esc(t.id)+'">详情 →</a></td></tr>').join("")+'</tbody></table>';
  return '<table><thead><tr><th>任务</th><th>状态</th><th>镜像数</th><th>同步耗时</th><th>创建时间</th><th></th></tr></thead><tbody>' + tasks.map(t => '<tr><td><a class="mono" href="/tasks/' + esc(t.id) + '">' + esc(t.id.slice(0,8)) + '</a><small>' + esc(t.items[0]?.source || "") + (t.items.length > 1 ? " 等" : "") + '</small></td><td>' + pill(t.status) + '</td><td>' + t.items.length + '</td><td>' + duration(t.elapsed_ms) + '</td><td>' + esc(date(t.created_at)) + '</td><td><a href="/tasks/' + esc(t.id) + '">查看详情 →</a></td></tr>').join("") + '</tbody></table>';
}
async function loadRegistries() { registries = await api("/api/registries"); }
function registryOptions(select, selected) {
  select.innerHTML = '<option value="">请选择仓库</option>' + registries.map(r => '<option value="' + esc(r.id) + '"' + (r.enabled ? "" : " disabled") + '>' + esc(r.name) + (r.enabled ? "" : "（已停用）") + '</option>').join("");
  if (selected) select.value = selected;
}
function renderRegistries() {
  const visible=pageRows(registries,"registries");
  $("registry-list").innerHTML = registries.length ? '<table><thead><tr><th>仓库</th><th>地址 / 命名空间</th><th>状态</th><th>操作</th></tr></thead><tbody>' + visible.map(r => '<tr><td><strong>' + esc(r.name) + '</strong><small>阿里云 ACR</small></td><td><code>' + esc(r.endpoint) + '</code><small>' + esc(r.namespace) + '</small></td><td><span class="pill ' + (r.enabled ? "success" : "") + '">' + (r.enabled ? "已启用" : "已停用") + '</span></td><td><button data-action="edit-registry" data-id="' + r.id + '">编辑</button><button data-action="toggle-registry" data-id="' + r.id + '">' + (r.enabled ? "停用" : "启用") + '</button><button data-action="delete-registry" data-id="' + r.id + '">移除</button></td></tr>').join("") + '</tbody></table>' : empty("添加第一个目标仓库", "配置阿里云 ACR 地址和凭据后，即可创建镜像同步规则。");
  renderPager("registries",registries.length);
}
async function registryList() { await loadRegistries(); renderRegistries(); }
function ruleTable(items) {
  if (!items.length) return empty("没有匹配的镜像规则", "添加规则，或调整名称和状态筛选条件。");
  return '<table class="rules-table"><thead><tr><th class="check-cell"><input id="select-all" type="checkbox" aria-label="选择全部可见启用规则"></th><th>镜像规则 / 源镜像</th><th>目标镜像 / 架构</th><th>同步状态</th><th>最近执行 / 耗时</th><th class="operations">操作</th></tr></thead><tbody>' + items.map(r => {
    const sync = r.sync || {status:"never"};
    return '<tr><td class="check-cell"><input type="checkbox" class="rule-select" value="' + esc(r.id) + '" aria-label="选择 ' + esc(r.name) + '"' + (r.enabled ? "" : " disabled") + (selectedRules.has(r.id) ? " checked" : "") + '></td><td><strong>' + esc(r.name) + '</strong><small class="mono">' + esc(r.source) + '</small>' + (r.enabled ? "" : '<span class="pill neutral">已停用</span>') + '</td><td><code title="' + esc(r.target) + '">' + esc(r.target) + '</code><small><span class="architecture">' + esc(platform(r.platform)) + '</span></small></td><td>' + pill(sync.status) + (sync.error ? '<small class="row-error" title="' + esc(sync.error) + '">' + esc(sync.error) + '</small>' : "") + (sync.warning ? '<small class="row-warning" title="'+esc(sync.warning)+'">'+esc(sync.warning)+'</small>' : '') + '</td><td><span class="time-text">' + esc(date(sync.updated_at)) + '</span><small>耗时 ' + duration(sync.elapsed_ms) + (sync.task_id ? ' · <a href="/tasks/' + esc(sync.task_id) + '">查看记录</a>' : "") + '</small></td><td class="operations"><button class="text-button" data-action="sync-rule" data-id="' + esc(r.id) + '"' + (r.enabled ? "" : " disabled") + '>同步</button><button class="text-button" data-action="edit-rule" data-id="' + esc(r.id) + '">编辑</button><button class="text-button secondary-text" data-action="toggle-rule" data-id="' + esc(r.id) + '">' + (r.enabled ? "停用" : "启用") + '</button><button class="text-button danger" data-action="delete-rule" data-id="' + esc(r.id) + '">移除</button></td></tr>';
  }).join("") + '</tbody></table>';
}
function renderRules() {
  const query = ($("rule-search")?.value || "").trim().toLowerCase(), status = $("rule-status-filter")?.value || "all";
  const successful = r => ["succeeded","unchanged"].includes(r.sync?.status);
  const failed = r => ["failed","dispatch_failed","needs_review"].includes(r.sync?.status);
  const active = r => r.sync && r.sync.status !== "never" && r.sync.status !== "unchanged" && !terminal(r.sync.status);
  const items = rules.filter(r => (!query || [r.name,r.source,r.target].some(v => v.toLowerCase().includes(query))) && (status === "all" || (status === "success" && successful(r)) || (status === "failed" && failed(r)) || (status === "active" && active(r)) || (status === "never" && (!r.sync || r.sync.status === "never"))));
  const visible=pageRows(items,"rules");
  renderPager("rules",items.length);
  $("rule-list").innerHTML = rules.length ? ruleTable(visible) : empty("添加第一个镜像规则", "填写源镜像，选择目标仓库和架构，即可开始同步。");
  const counts = {"rules-total":rules.length,"rules-success":rules.filter(successful).length,"rules-active":rules.filter(active).length,"rules-failed":rules.filter(failed).length};
  for (const [id,count] of Object.entries(counts)) if ($(id)) $(id).textContent = count;
  if ($("rules-count")) $("rules-count").textContent = "共 " + rules.length + " 条规则";
  if ($("sync-selected")) $("sync-selected").textContent = selectedRules.size ? "同步所选（" + selectedRules.size + "）" : "同步所选";
  const selectAll=$("select-all"); if(selectAll) {const enabled=visible.filter(r=>r.enabled);selectAll.checked=enabled.length>0&&enabled.every(r=>selectedRules.has(r.id));selectAll.indeterminate=!selectAll.checked&&enabled.some(r=>selectedRules.has(r.id));}
  $("select-all")?.addEventListener("change", e => { document.querySelectorAll(".rule-select:not(:disabled)").forEach(c => {c.checked = e.target.checked; if(c.checked) selectedRules.add(c.value); else selectedRules.delete(c.value);}); renderRules(); });
}
async function ruleList() {
  rules = await api("/api/rules");
  const enabled = new Set(rules.filter(r => r.enabled).map(r => r.id));
  for (const id of selectedRules) if (!enabled.has(id)) selectedRules.delete(id);
  renderRules();
}
function overviewRules(items) {
  if(!items.length) return empty("还没有镜像规则","点击添加镜像规则，开始第一次同步。");
  return '<table><thead><tr><th>镜像规则 / 源镜像</th><th>同步状态</th><th>同步耗时</th><th></th></tr></thead><tbody>'+items.map(r=>'<tr><td><strong>'+esc(r.name)+'</strong><small class="mono">'+esc(r.source)+'</small></td><td>'+pill(r.sync?.status||"never")+'</td><td>'+duration(r.sync?.elapsed_ms)+'</td><td><a href="'+(r.sync?.task_id?'/tasks/'+esc(r.sync.task_id):'/rules')+'">查看详情 →</a></td></tr>').join("")+'</tbody></table>';
}
function inventoryTable(items) {
  if (!items.length) return empty("当前查询范围内没有镜像标签", "可填写镜像名称后按名称查询，或切换目标仓库。");
  return '<table class="images-table"><thead><tr><th>镜像名称 / 标签</th><th>实际架构</th><th>镜像摘要</th><th>操作</th></tr></thead><tbody>' + items.map(i => '<tr><td><strong>' + esc(i.repository) + '</strong><small><span class="tag">' + esc(i.tag) + '</span></small></td><td>' + (i.platforms.length ? '<div class="platform-tags">' + i.platforms.map(p => '<span class="architecture">' + esc(p) + '</span>').join("") + '</div>' : '<span class="muted">架构未知</span>') + '</td><td><code class="digest" title="' + esc(i.digest || "") + '">' + esc(i.digest || "—") + '</code>' + (i.error ? '<small class="row-error">' + esc(i.error) + '</small>' : "") + '</td><td><button class="text-button" data-action="copy-reference" data-reference="' + esc(i.reference) + '">复制引用</button></td></tr>').join("") + '</tbody></table>';
}
function renderInventory(view) {
  inventoryData = view; resetPage("images");
  $("image-scope").textContent = ({catalog:"仓库可发现镜像",known:"已配置镜像（部分范围）",repository:"指定镜像"}[view.scope] || "查询结果") + " · " + view.images.length + " 个标签";
  $("image-warnings").textContent = view.warnings.join("\n"); $("image-warnings").hidden = !view.warnings.length;
  if ($("image-updated")) $("image-updated").textContent = "查询于 " + date(view.checked_at) + (view.complete ? "" : " · 部分结果");
  if ($("repository-names")) $("repository-names").innerHTML = view.repositories.map(n => '<option value="' + esc(n) + '"></option>').join("");
  filterInventory();
}
function filterInventory() {
  const query = ($("image-search")?.value || "").trim().toLowerCase();
  const all = inventoryData?.images || [];
  if (!all.length && inventoryData && !inventoryData.complete) {
    renderPager("images",0);
    $("image-list").innerHTML = empty("查询未完成，未能获取镜像列表", "请查看上方提示，检查仓库凭据，或填写镜像名称后按名称查询。");
    return;
  }
  const items=all.filter(i => [i.repository,i.tag,...i.platforms].some(v => v.toLowerCase().includes(query)));
  const visible=pageRows(items,"images");renderPager("images",items.length);
  $("image-list").innerHTML = all.length && !items.length ? empty("没有匹配的镜像", "请调整名称、标签或架构筛选条件。") : inventoryTable(visible);
}
async function loadInventory() {
  const request=++inventoryRequest,id=$("image-registry").value;
  resetPage("images");inventoryData=null;renderPager("images",0);
  $("image-warnings").hidden=true;$("image-updated").textContent="";
  if(!id) {
    $("query-images").disabled=false;$("image-scope").textContent="远端镜像";
    $("image-list").innerHTML=empty("请先添加或选择目标仓库","在目标仓库中填写地址和凭据，即可查看远端镜像。");return;
  }
  const repository=$("image-repository").value.trim();
  $("query-images").disabled=true;$("image-list").innerHTML='<p class="loading">正在查询远端仓库，请稍候…</p>';$("image-scope").textContent="正在查询";
  try {
    const result=await api("/api/registries/"+encodeURIComponent(id)+"/images"+(repository?"?repository="+encodeURIComponent(repository):""));
    if(request===inventoryRequest)renderInventory(result);
  } catch(e) {
    if(request!==inventoryRequest)return;
    $("image-list").innerHTML=empty("查询失败",e.message);$("image-scope").textContent="查询未完成";throw e;
  } finally {if(request===inventoryRequest)$("query-images").disabled=false;}
}
async function copyReference(text) {
  if (navigator.clipboard?.writeText) await navigator.clipboard.writeText(text);
  else { const field=document.createElement("textarea");field.value=text;field.readOnly=true;document.body.append(field);field.select();const ok=document.execCommand("copy");field.remove();if(!ok) throw new Error("复制失败，请手工选择镜像引用后复制"); }
  notice("镜像引用已复制");
}
function connectionStatus(ready) {
  $("connection-status").innerHTML = '<span class="pill '+(ready?"success":"neutral")+'">'+(ready?"已就绪":"待初始化")+'</span>';
  $("connection-hint").textContent = ready ? "GitHub 连接已就绪，可以开始同步镜像。" : "保存并初始化连接后，即可开始同步。";
}
function previewTarget() {
  const form = $("rule-form"), r = registries.find(r => r.id === form.elements.registry_id.value);
  $("target-preview").textContent = r ? r.endpoint + "/" + r.namespace + "/" + form.elements.target_repository.value + ":" + form.elements.target_tag.value : "请选择目标仓库";
}
function suggestRule() {
  const f = $("rule-form"), raw = f.elements.source.value.trim(), withoutDigest = raw.split("@")[0], last = withoutDigest.split("/").pop() || "";
  const colon = last.lastIndexOf(":"), tag = colon >= 0 ? last.slice(colon+1) : "latest", name = colon >= 0 ? last.slice(0,colon) : last;
  if (!f.elements.name.value) f.elements.name.value = raw;
  if (!f.elements.target_repository.value) f.elements.target_repository.value = name;
  if (!f.elements.id.value && !f.elements.target_tag.dataset.edited) f.elements.target_tag.value = tag + (f.elements.platform.value === "all" ? "" : "-" + f.elements.platform.value.split("/")[1]);
  previewTarget();
}
function uuid() {
  if (crypto.randomUUID) return crypto.randomUUID();
  const b = crypto.getRandomValues(new Uint8Array(16)); b[6] = b[6]&15|64; b[8] = b[8]&63|128;
  const h = Array.from(b,x => x.toString(16).padStart(2,"0")).join(""); return h.slice(0,8)+"-"+h.slice(8,12)+"-"+h.slice(12,16)+"-"+h.slice(16,20)+"-"+h.slice(20);
}
async function sync(ids) {
  if (!ids.length || ids.length > 20) throw new Error("每次请选择 1 至 20 条规则");
  const task = await api("/api/tasks","POST",{rule_ids:ids,request_key:uuid()});
  location.assign("/tasks/" + task.id);
}
async function taskDetail() {
  const id = document.body.dataset.taskId, data = await api("/api/tasks/" + id), t = data.task, result = data.result;
  $("task-status").innerHTML = pill(t.status) + '<p class="muted" style="margin-top:14px">创建于 ' + esc(date(t.created_at)) + ' · 第 ' + t.run_attempt + ' 次执行 · 同步耗时 ' + duration(t.elapsed_ms) + '</p>' + (t.error ? '<p class="notice error">' + esc(t.error) + '</p>' : "") + (data.warning ? '<p class="help">' + esc(data.warning) + '</p>' : "") + (result ? '<p class="muted">已同步 ' + result.summary.synced + ' · 内容一致 ' + result.summary.unchanged + ' · 失败 ' + result.summary.failed + '</p>' : "");
  $("task-actions").innerHTML = (t.run_url ? '<a class="button" target="_blank" rel="noopener noreferrer" href="' + esc(t.run_url) + '">在 GitHub 查看 ↗</a>' : "") + (terminal(t.status) ? '<button data-action="retry-task" data-id="' + t.id + '">重新同步</button>' : "") + (["dispatch_unknown","needs_review"].includes(t.status) ? '<button data-action="release-task" data-id="' + t.id + '">人工解除</button>' : "");
  taskDetailItems = result ? result.items : t.items; renderTaskItems();
}
function renderTaskItems() {
  const items=pageRows(taskDetailItems,"items");renderPager("items",taskDetailItems.length);
  $("task-items").innerHTML = '<table><thead><tr><th>源镜像 → 目标镜像</th><th>平台</th><th>结果</th><th>耗时</th><th>Digest / 错误</th></tr></thead><tbody>' + items.map(i => '<tr><td><code>' + esc(i.source) + '</code><small>→ ' + esc(i.target) + '</small></td><td>' + esc(platform(i.platform)) + '</td><td>' + esc({synced:"已同步",unchanged:"内容一致，跳过",failed:"失败"}[i.outcome] || "等待结果") + '</td><td>' + duration(i.elapsed_ms) + '</td><td><code title="' + esc(i.target_digest || "") + '">' + esc(i.target_digest ? i.target_digest.slice(0,23)+"…" : i.error || "—") + '</code></td></tr>').join("") + '</tbody></table>';
}
async function start() {
  const session = await api("/api/session"); csrf = session.csrf;
  $("logout")?.addEventListener("click", action(async () => { await api("/api/logout","POST"); location.assign("/login"); }));
  document.addEventListener("click", action(async (event) => {
    const close = event.target.closest("[data-close]"); if (close) { closeEditor(close.dataset.close); return; }
    const pageButton=event.target.closest("[data-page-key]");
    if(pageButton&&!pageButton.disabled) {const key=pageButton.dataset.pageKey,state=paging[key];if(!state)return;state.page=Number(pageButton.dataset.pageNumber);await renderPage(key);return;}
    const button = event.target.closest("[data-action]"); if (!button || button.disabled) return;
    const {id,action:kind} = button.dataset; button.disabled = true;
    try {
      if (kind === "copy-reference") { await copyReference(button.dataset.reference); }
      else if (kind.endsWith("registry")) {
        const r = registries.find(r => r.id === id);
        if (kind === "edit-registry") { const f = $("registry-form"); f.reset(); fill(f,r); $("registry-editor-title").textContent = "编辑仓库"; openEditor("registry-editor"); }
        if (kind === "toggle-registry") { await api("/api/registries/"+id,"PUT",{...r,enabled:!r.enabled}); await registryList(); }
        if (kind === "delete-registry" && confirm("移除此仓库配置？被镜像规则引用的仓库不能移除。")) { await api("/api/registries/"+id,"DELETE"); await registryList(); }
      } else if (kind.endsWith("rule")) {
        const r = rules.find(r => r.id === id);
        if (kind === "edit-rule") { const f = $("rule-form"); f.reset(); fill(f,r); registryOptions(f.elements.registry_id,r.registry_id); delete f.elements.target_tag.dataset.edited; $("rule-editor-title").textContent = "编辑镜像规则"; previewTarget(); openEditor("rule-editor"); }
        if (kind === "toggle-rule") { await api("/api/rules/"+id,"PUT",{...r,enabled:!r.enabled}); await ruleList(); }
        if (kind === "delete-rule" && confirm("移除此同步规则？远程镜像会保留。")) { await api("/api/rules/"+id,"DELETE"); await ruleList(); }
        if (kind === "sync-rule") await sync([id]);
      } else if (kind === "retry-task") { const task = await api("/api/tasks/"+id+"/retry","POST"); location.assign("/tasks/"+task.id); }
      else if (kind === "release-task" && confirm("请先到 GitHub 核实运行状态。解除后 GitHub 可能仍在执行，确定解除并允许后续任务吗？")) { await api("/api/tasks/"+id+"/release","POST",{confirm:true}); await taskDetail(); }
    } finally { button.disabled = false; }
  }));
  document.addEventListener("change",action(async event=>{
    const select=event.target.closest("[data-page-size]");if(!select)return;
    const key=select.dataset.pageSize;if(!paging[key])return;const size=Number(select.value);if(![5,10,20,50].includes(size))return;
    paging[key].size=size;resetPage(key);await renderPage(key);
  }));
  for(const id of ["rule-editor","registry-editor"]) $(id)?.addEventListener("close",()=>{$(id).hidden=true;});
  if (page === "login") {
    if (new URLSearchParams(location.search).get("password_changed") === "1") notice("密码已修改，请使用新密码登录");
    $("login-form").addEventListener("submit",action(async () => { await api("/api/login","POST",{password:$("password").value}); location.assign("/"); })); return;
  }
  if (page === "account") {
    $("password-form").addEventListener("submit",action(async () => {
      const form = $("password-form"), data = formData(form);
      validatePasswordChange(data);
      await api("/api/account/password","POST",data);
      form.reset();
      location.assign("/login?password_changed=1");
    }));
  }
  if (page === "dashboard") {
    const [r,images,tasks] = await Promise.all([api("/api/registries"),api("/api/rules"),api("/api/tasks")]);
    const counts = [images.length,images.filter(r => ["succeeded","unchanged"].includes(r.sync?.status)).length,tasks.filter(t => !terminal(t.status)).length,r.length];
    document.querySelectorAll(".metric strong").forEach((el,i) => el.textContent = counts[i]);
    $("recent-tasks").innerHTML = taskTable(tasks.slice(0,5),true);
    if ($("recent-rules")) $("recent-rules").innerHTML = overviewRules(images.slice(0,5));
  }
  if (page === "settings") {
    const settings = await api("/api/settings");
    if (settings) { fill($("settings-form"),settings); $("token-state").textContent = settings.has_token ? "Token 已保存，留空保留。需要该仓库的 Actions、Secrets 读写权限；私有仓库还需 Contents 只读权限。" : "需要该仓库的 Actions、Secrets 读写权限；私有仓库还需 Contents 只读权限。"; }
    connectionStatus(settings?.ready);
    $("settings-form").addEventListener("submit",action(async () => {
      await api("/api/settings","PUT",formData($("settings-form"))); $("token").value = ""; connectionStatus(false);
      notice("正在初始化 GitHub 连接，请稍候…","info");
      const result = await api("/api/settings/initialize","POST");
      const current = await api("/api/settings"); connectionStatus(current?.ready); notice(result.message);
    }));
  }
  if (page === "images") {
    await loadRegistries(); registryOptions($("image-registry"));
    const first=registries.find(r=>r.enabled); if(first) $("image-registry").value=first.id;
    $("image-query-form").addEventListener("submit",action(loadInventory));
    $("image-registry").addEventListener("change",action(async()=>{ $("image-repository").value=""; await loadInventory(); }));
    $("image-search").addEventListener("input",()=>{resetPage("images");filterInventory();});
    await loadInventory();
  }
  if (page === "registries") {
    await registryList();
    $("new-registry").addEventListener("click",() => { $("registry-form").reset(); $("registry-form").elements.id.value=""; $("registry-editor-title").textContent="添加仓库"; openEditor("registry-editor"); });
    $("registry-form").addEventListener("submit",action(async () => { const f = $("registry-form"), id=f.elements.id.value; await api("/api/registries"+(id?"/"+id:""),id?"PUT":"POST",formData(f)); f.reset(); closeEditor("registry-editor"); await registryList(); notice("仓库配置已保存"); }));
  }
  if (page === "rules") {
    await loadRegistries(); await ruleList(); registryOptions($("registry-id"));
    $("new-rule").addEventListener("click",() => { const f=$("rule-form"); f.reset(); f.elements.id.value=""; delete f.elements.target_tag.dataset.edited; registryOptions(f.elements.registry_id); $("rule-editor-title").textContent="添加镜像规则"; previewTarget(); openEditor("rule-editor"); });
    $("rule-form").addEventListener("input",previewTarget); $("source").addEventListener("change",suggestRule); $("platform").addEventListener("change",suggestRule); $("target-tag").addEventListener("input",() => $("target-tag").dataset.edited="true");
    $("rule-form").addEventListener("submit",action(async () => { const f=$("rule-form"),id=f.elements.id.value; await api("/api/rules"+(id?"/"+id:""),id?"PUT":"POST",formData(f)); f.reset(); closeEditor("rule-editor"); await ruleList(); notice("镜像规则已保存"); }));
    $("sync-selected").addEventListener("click",action(() => sync(Array.from(selectedRules))));
    $("rule-search").addEventListener("input",()=>{resetPage("rules");renderRules();}); $("rule-status-filter").addEventListener("change",()=>{resetPage("rules");renderRules();});
    $("refresh-rules").addEventListener("click",action(ruleList));
    document.addEventListener("change",event=>{if(event.target.classList.contains("rule-select")){if(event.target.checked)selectedRules.add(event.target.value);else selectedRules.delete(event.target.value);renderRules();}});
    setInterval(()=>ruleList().catch(e=>notice(e.message,"error")),15000);
  }
  if (page === "tasks") {
    const refresh=taskList;
    await refresh(); $("refresh-tasks").addEventListener("click",action(refresh)); setInterval(() => refresh().catch(e => notice(e.message,"error")),15000);
  }
  if (page === "task_detail") {
    await taskDetail(); setInterval(() => taskDetail().catch(e => notice(e.message,"error")),15000);
    $("load-logs").addEventListener("click",action(async () => { const r=await api("/api/tasks/"+document.body.dataset.taskId+"/logs"); $("task-logs").textContent=r.logs; $("task-logs").hidden=false; }));
  }
}
start().catch(e => notice(e.message,"error"));
