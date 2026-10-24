const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

async function render(page, responses) {
  if(Object.hasOwn(responses,'/api/tasks')) {
    const items=responses['/api/tasks'];responses['/api/tasks/page?number=1&size=10']={items,total:items.length,page:1,page_size:10,pages:1};
  }
  const elements = new Map();
  const element = id => {
    if (!elements.has(id)) elements.set(id, {innerHTML:'',textContent:'',hidden:true,dataset:{},contains(){return false;},querySelector(){return null;},addEventListener(){}});
    return elements.get(id);
  };
  const context = vm.createContext({
    document:{body:{dataset:{page,taskId:'task-1'}},getElementById:element,addEventListener(){},querySelectorAll(){return [];}},
    fetch:async url => {
      assert.ok(Object.hasOwn(responses,url),`unexpected request ${url}`);
      return {ok:true,status:200,json:async()=>responses[url]};
    },
    setInterval(){},
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname,'../static/app.js'),'utf8'),context);
  await new Promise(setImmediate);
  return {elements, context};
}
const task = {id:'task-1',status:'failed',created_at:1,run_attempt:1,elapsed_ms:62500,items:[{source:'nginx:latest',target:'acr.example/demo/nginx:latest',platform:'all'}]};
const session = {csrf:'fixture'};

test('password form counts Unicode characters consistently and rejects mismatched confirmation', async()=>{
  const {context}=await render('account',{'/api/session':session});
  for(const count of [12,128]) {
    context.candidate={new_password:'🔒'.repeat(count),confirm_password:'🔒'.repeat(count)};
    assert.doesNotThrow(()=>vm.runInContext('validatePasswordChange(candidate)',context));
  }
  for(const count of [6,129]) {
    context.candidate={new_password:'🔒'.repeat(count),confirm_password:'🔒'.repeat(count)};
    assert.throws(()=>vm.runInContext('validatePasswordChange(candidate)',context),/12 至 128/);
  }
  context.candidate={new_password:'valid-new-password',confirm_password:'different-password'};
  assert.throws(()=>vm.runInContext('validatePasswordChange(candidate)',context),/不一致/);
});

test('task list displays execution duration and keeps unknown duration distinct from zero', async()=>{
  const rows=[task,{...task,id:'zero',elapsed_ms:0},{...task,id:'queued',status:'queued',elapsed_ms:null}];
  const {elements}=await render('tasks',{'/api/session':session,'/api/tasks':rows});
  const html=elements.get('task-list').innerHTML;
  assert.match(html,/同步耗时/);
  assert.match(html,/1 分 2 秒/);
  assert.match(html,/0 毫秒/);
  assert.match(html,/—/);
});

test('task details show batch and individual image durations',async()=>{
  const item={...task.items[0],elapsed_ms:850,outcome:'failed',error:'fixture'};
  const {elements}=await render('task_detail',{'/api/session':session,'/api/tasks/task-1':{task,result:{items:[item],summary:{synced:0,unchanged:0,failed:1}}}});
  assert.match(elements.get('task-status').innerHTML,/同步耗时.*1 分 2 秒/);
  assert.match(elements.get('task-items').innerHTML,/850 毫秒/);
});

test('duration formatting handles milliseconds, seconds, minutes, hours and missing values',async()=>{
  const {context}=await render('tasks',{'/api/session':session,'/api/tasks':[]});
  for (const [input,expected] of [[null,'—'],[undefined,'—'],[-1,'—'],[0,'0 毫秒'],[999,'999 毫秒'],[1000,'1 秒'],[59999,'59 秒'],[60000,'1 分 0 秒'],[3661234,'1 小时 1 分 1 秒']]) {
    context.input=input;
    assert.equal(vm.runInContext('duration(input)',context),expected);
  }
});


test('rule table shows per-image outcomes, failure detail and last task without inventing success',async()=>{
  const {context}=await render('tasks',{'/api/session':session,'/api/tasks':[]});
  context.fixture=[
    {id:'r1',name:'Nginx',source:'nginx:latest',target:'acr/demo/nginx:latest',platform:'all',enabled:true,sync:{status:'succeeded',task_id:'ok-task',updated_at:1,elapsed_ms:1250}},
    {id:'r2',name:'Redis',source:'redis:latest',target:'acr/demo/redis:latest',platform:'linux/arm64',enabled:true,sync:{status:'failed',task_id:'bad-task',updated_at:1,error:'<img src=x onerror=alert(1)>',elapsed_ms:0}},
    {id:'r3',name:'Never',source:'alpine',target:'acr/demo/alpine:latest',platform:'all',enabled:true,sync:{status:'never'}}
  ];
  const html=vm.runInContext('ruleTable(fixture)',context);
  assert.match(html,/同步成功/); assert.match(html,/同步失败/); assert.match(html,/未同步/);
  assert.match(html,/1 秒/); assert.match(html,/0 毫秒/); assert.match(html,/\/tasks\/ok-task/);
  assert.match(html,/&lt;img/); assert.doesNotMatch(html,/<img src=x/);
});

test('remote image table displays actual platform list and metadata failure separately',async()=>{
  const {context}=await render('tasks',{'/api/session':session,'/api/tasks':[]});
  context.fixture=[{repository:'nginx',tag:'v1',reference:'acr/demo/nginx:v1',platforms:['linux/amd64','linux/arm64/v8'],digest:'sha256:'+'a'.repeat(64),error:null},{repository:'redis',tag:'missing',reference:'acr/demo/redis:missing',platforms:[],digest:null,error:'<script>bad</script>'}];
  const html=vm.runInContext('inventoryTable(fixture)',context);
  assert.match(html,/linux\/amd64/);assert.match(html,/linux\/arm64\/v8/);assert.match(html,/架构未知/);
  assert.match(html,/&lt;script&gt;/);assert.doesNotMatch(html,/<script>/);assert.match(html,/复制引用/);
});

test('inventory scope and warnings remain visible when listing is partial',async()=>{
  const {context,elements}=await render('tasks',{'/api/session':session,'/api/tasks':[]});
  context.fixture={scope:'known',complete:false,checked_at:1,repositories:[],images:[],warnings:['不支持完整目录查询']};
  vm.runInContext('renderInventory(fixture)',context);
  assert.match(elements.get('image-scope').textContent,/已配置/);
  assert.match(elements.get('image-warnings').textContent,/不支持完整目录查询/);
  assert.match(elements.get('image-list').innerHTML,/按名称查询/);
});

test('failed remote reads are never presented as an empty repository',async()=>{
  const {context,elements}=await render('tasks',{'/api/session':session,'/api/tasks':[]});
  context.fixture={scope:'known',complete:false,checked_at:1,repositories:['nginx'],images:[],warnings:['nginx：仓库拒绝访问，请检查密码']};
  vm.runInContext('renderInventory(fixture)',context);
  assert.doesNotMatch(elements.get('image-list').innerHTML,/没有镜像标签/);
  assert.match(elements.get('image-list').innerHTML,/查询未完成|未能获取/);
});


test('pagination slices rows without gaps and clamps the final page after deletion',async()=>{
  const {context}=await render('tasks',{'/api/session':session,'/api/tasks':[]});
  context.rows=Array.from({length:23},(_,i)=>i+1);
  assert.equal(vm.runInContext('pageRows(rows,"rules").join(",")',context),'1,2,3,4,5,6,7,8,9,10');
  vm.runInContext('paging.rules.page=3',context);
  assert.equal(vm.runInContext('pageRows(rows,"rules").join(",")',context),'21,22,23');
  assert.equal(vm.runInContext('pageRows(rows.slice(0,20),"rules").join(",")',context),'11,12,13,14,15,16,17,18,19,20');
  assert.equal(vm.runInContext('paging.rules.page',context),2);
  assert.equal(vm.runInContext('pageRows([],"rules").length',context),0);
  assert.equal(vm.runInContext('paging.rules.page',context),1);
});

test('rule pagination preserves cross-page selection and resets page when filters change',async()=>{
  const {context,elements}=await render('tasks',{'/api/session':session,'/api/tasks':[]});
  context.rows=Array.from({length:23},(_,i)=>({id:'r'+i,name:'Rule '+i,source:'nginx:'+i,target:'acr/demo/nginx:'+i,platform:'all',enabled:true,sync:{status:'never'}}));
  vm.runInContext('rules=rows;selectedRules.add("r0");paging.rules.page=2;renderRules()',context);
  assert.match(elements.get('rule-list').innerHTML,/Rule 10/);assert.doesNotMatch(elements.get('rule-list').innerHTML,/>Rule 0</);
  vm.runInContext('paging.rules.page=1;renderRules()',context);
  assert.match(elements.get('rule-list').innerHTML,/value="r0"[^>]*checked/);
  vm.runInContext('paging.rules.page=3;resetPage("rules");renderRules()',context);
  assert.equal(vm.runInContext('paging.rules.page',context),1);
});

test('inventory pagination shows only the selected slice and keeps the queried total',async()=>{
  const {context,elements}=await render('tasks',{'/api/session':session,'/api/tasks':[]});
  context.view={scope:'catalog',complete:true,checked_at:1,repositories:['nginx'],warnings:[],images:Array.from({length:12},(_,i)=>({repository:'nginx',tag:'tag-'+i,reference:'acr/demo/nginx:tag-'+i,platforms:['linux/amd64'],digest:null,error:null}))};
  vm.runInContext('renderInventory(view);paging.images.page=2;filterInventory()',context);
  assert.match(elements.get('image-list').innerHTML,/tag-10/);assert.doesNotMatch(elements.get('image-list').innerHTML,/>tag-0</);
  assert.match(elements.get('images-pagination').innerHTML,/12/);
});

test('save errors remain inside the active editor dialog',async()=>{
  const {context,elements}=await render('tasks',{'/api/session':session,'/api/tasks':[]});
  context.editorNotice={hidden:true,textContent:'',className:''};
  context.document.querySelector=()=>context.editorNotice;
  vm.runInContext('notice("目标标签无效","error")',context);
  assert.equal(context.editorNotice.hidden,false);assert.equal(context.editorNotice.textContent,'目标标签无效');
  assert.notEqual(elements.get('notice')?.textContent,'目标标签无效');
});

test('clearing the registry cancels stale results and clears inventory pagination',async()=>{
  const {context,elements}=await render('tasks',{'/api/session':session,'/api/tasks':[]});
  let complete;
  context.fetch=()=>new Promise(resolve=>{complete=resolve;});
  context.document.getElementById('image-registry').value='registry-1';context.document.getElementById('image-repository').value='';
  const previous=vm.runInContext('loadInventory()',context);
  context.document.getElementById('image-registry').value='';
  await vm.runInContext('loadInventory()',context);
  complete({ok:true,status:200,json:async()=>({scope:'catalog',complete:true,checked_at:1,repositories:['stale'],warnings:[],images:[{repository:'stale',tag:'old',reference:'acr/demo/stale:old',platforms:[],digest:null,error:null}]})});
  await previous;
  assert.doesNotMatch(elements.get('image-list').innerHTML,/stale/);
  assert.match(elements.get('images-pagination').innerHTML,/共 0 条/);
  assert.equal(elements.get('query-images').disabled,false);
});
