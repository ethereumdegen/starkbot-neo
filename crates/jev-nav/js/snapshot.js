(() => {
  if (!document.body) return null;
  const cache = window.__jevFast ||= {ids:new WeakMap(),nodes:new Map(),meta:new WeakMap(),next:1};
  cache.meta ||= new WeakMap();
  const identity=e=>{
    if (!cache.ids.has(e)) cache.ids.set(e,cache.next++);
    const id=cache.ids.get(e); cache.nodes.set(id,e); return id;
  };
  for (const [id,e] of cache.nodes) if (!e.isConnected) cache.nodes.delete(id);
  const parent=e=>e?.assignedSlot || e?.parentElement || e?.getRootNode?.().host || null;
  const closest=(e,selector)=>{
    for (let n=e;n;n=parent(n)) if (n.matches?.(selector)) return n;
    return null;
  };
  const contains=(ancestor,node)=>{
    for (let n=node;n;n=parent(n)) if (n===ancestor) return true;
    return false;
  };
  // Card and bank fields are dropped the way password fields are: never a
  // candidate, never in the page key, so their digits cannot reach the model,
  // the history or a trace. A human types these (10 §7).
  const paymentNames=/card.?num|cardno|cc.?num|cvc|cvv|csc|security.?code|iban|sort.?code|routing|account.?num/i;
  const payment=e=>{
    if (!['INPUT','TEXTAREA'].includes(e.tagName)) return false;
    const hint=(e.getAttribute('autocomplete')||'').toLowerCase();
    if (hint.split(/\s+/).some(token=>token.startsWith('cc-'))) return true;
    return paymentNames.test([e.name,e.id,e.getAttribute('placeholder'),
      e.getAttribute('aria-label')].filter(Boolean).join(' '));
  };
  const safe=e=>!['password','hidden'].includes(e.type) && !payment(e);
  const visible=e=>!closest(e,'[aria-hidden="true"],[inert]') &&
    e.checkVisibility({checkOpacity:true,checkVisibilityCSS:true});
  const byId=(e,id)=>{
    const root=e.getRootNode();
    return root.getElementById?.(id) || e.ownerDocument.getElementById(id);
  };
  const name=(e,seen=new Set())=>{
    if (!e || seen.has(e)) return '';
    seen.add(e);
    const referenced=(e.getAttribute?.('aria-labelledby')||'').split(/\s+/)
      .map(id=>name(byId(e,id),seen)).filter(Boolean).join(' ');
    return referenced || e.getAttribute?.('aria-label') ||
      [...(e.labels||[])].map(l=>name(l,seen)).filter(Boolean).join(' ') ||
      (['button','submit','reset'].includes(e.type) ? e.value : '') || e.getAttribute?.('alt') ||
      (e.tagName==='INPUT' ? '' : [...(e.childNodes||[])].map(n=>n.nodeType===3 ? n.textContent :
        n.nodeType===1 && n.getAttribute('aria-hidden')!=='true' ? name(n,seen) : '').join(' ').trim()) ||
      e.getAttribute?.('title') || e.getAttribute?.('placeholder') || '';
  };
  const roles=['button','link','checkbox','radio','switch','tab','menuitem','menuitemradio',
    'option','gridcell','combobox','textbox','searchbox','spinbutton'];
  const selector='a[href],button,input,textarea,select,summary,[contenteditable]:not([contenteditable="false"]),'+
    roles.map(role=>'[role="'+role+'"]').join(',');
  const role=e=>{
    const explicit=e.getAttribute('role');
    if (roles.includes(explicit)) return explicit;
    if (e.tagName==='BUTTON' || e.tagName==='SUMMARY') return 'button';
    if (e.tagName==='A') return 'link';
    if (e.tagName==='SELECT') return 'combobox';
    if (e.tagName==='TEXTAREA' || e.isContentEditable) return 'textbox';
    if (e.tagName==='INPUT') {
      if (e.type==='file') return 'file';
      if (['checkbox','radio'].includes(e.type)) return e.type;
      if (['button','submit','reset','image'].includes(e.type)) return 'button';
      if (e.type==='search') return 'searchbox';
      if (e.type==='number') return 'spinbutton';
      if (['text','email','url','tel'].includes(e.type)) return 'textbox';
    }
    return null;
  };
  // Well-known challenge widgets. A captcha is not something to solve or
  // click at: the run hands the page back to the user (10 §7).
  const captchaSrc=/recaptcha|hcaptcha|turnstile|arkoselabs|funcaptcha|geetest|px-captcha/i;
  const elements=[],textRoots=[]; let crossOriginFrames=0,captchaFrames=0;
  const collectRoot=(root,doc,ox,oy)=>{
    textRoots.push({root,ox,oy});
    const found=[...root.querySelectorAll('*')];
    for (const e of found) {
      cache.meta.set(e,{doc,ox,oy});
      elements.push(e);
      if (e.shadowRoot) collectRoot(e.shadowRoot,doc,ox,oy);
      if (e.tagName==='IFRAME' && visible(e)) {
        crossOriginFrames++;
        if (captchaSrc.test(e.getAttribute('src')||'')) captchaFrames++;
      }
    }
  };
  cache.meta.set(document.documentElement,{doc:document,ox:0,oy:0});
  collectRoot(document,document,0,0);
  const rect=e=>{
    const r=e.getBoundingClientRect(),m=cache.meta.get(e)||{ox:0,oy:0};
    return {x:r.x+m.ox,y:r.y+m.oy,w:r.width,h:r.height,localX:r.x+r.width/2,localY:r.y+r.height/2};
  };
  const deepHit=(doc,x,y)=>{
    let hit=doc.elementFromPoint(x,y);
    while (hit?.shadowRoot) {
      const deeper=hit.shadowRoot.elementFromPoint(x,y);
      if (!deeper || deeper===hit) break;
      hit=deeper;
    }
    return hit;
  };
  cache.pageKey=()=>[performance.timeOrigin,location.href,scrollX,scrollY,innerWidth,innerHeight,
    elements.filter(e=>['INPUT','TEXTAREA','SELECT'].includes(e.tagName) && safe(e))
      .map(e=>[identity(e),e.value,e.checked,e.selectedIndex,e.disabled,e.readOnly])];
  cache.guard=e=>{
    if (!e?.isConnected || (e.type!=='file' && !visible(e))) return null;
    const scope=closest(e,'form,dialog,[role="dialog"],article,li,tr,[role="row"]') || parent(e);
    return [identity(e),role(e),name(e),e.type==='file'?null:e.value??null,e.checked??null,
      e.selectedIndex??null,e.readOnly??null,e.matches(':disabled'),e.getAttribute('aria-disabled'),
      e.getAttribute('aria-expanded'),e.getAttribute('aria-checked'),e.getAttribute('aria-selected'),
      e.getAttribute('href'),scope?.innerText?.slice(0,6000)||''];
  };
  cache.resolve=action=>{
    const e=cache.nodes.get(action.node);
    if (!e?.isConnected || e.matches(':disabled') || closest(e,'[aria-disabled="true"],[inert]')) return null;
    if (action.kind==='upload') return e.type==='file' ? {upload:true} : null;
    if (!visible(e) || action.kind==='fill' && (e.readOnly || e.getAttribute('aria-readonly')==='true')) return null;
    const r=rect(e),m=cache.meta.get(e)||{doc:document,ox:0,oy:0};
    const x=r.x+r.w/2,y=r.y+r.h/2;
    if (!r.w || !r.h || x<0 || y<0 || x>=innerWidth || y>=innerHeight) return null;
    const hit=deepHit(m.doc,r.localX,r.localY);
    if (!contains(e,hit)) return null;
    if (action.kind==='select') {
      if (e.tagName!=='SELECT' || ![...e.options].some(o=>o.value===action.value &&
          !o.disabled && !o.closest('optgroup[disabled]'))) return null;
      e.value=action.value;
      e.dispatchEvent(new Event('input',{bubbles:true}));
      e.dispatchEvent(new Event('change',{bubbles:true}));
    }
    return {x,y};
  };
  const actions=[];
  for (const e of elements) {
    if (!e.matches?.(selector) || !safe(e) || e.matches(':disabled') ||
        closest(e,'[aria-disabled="true"]')) continue;
    const rname=role(e),isFile=e.tagName==='INPUT' && e.type==='file';
    if (!rname || !isFile && !visible(e)) continue;
    const r=rect(e),x=r.x+r.w/2,y=r.y+r.h/2;
    if (!isFile && (r.w<=0 || r.h<=0 || x<0 || y<0 || x>=innerWidth || y>=innerHeight)) continue;
    if (rname==='gridcell' && e.querySelector('button,[role="button"]')) continue;
    // `href` is what the deterministic layer checks a click against: a denied
    // host, or a destination that is not a web page at all. Resolved
    // absolute, because a relative one says nothing about origin.
    const base={node:identity(e),role:rname,label:name(e)||rname,
      rect:{x:r.x,y:r.y,w:r.w,h:r.h},frame:e.ownerDocument===document?null:e.ownerDocument.URL,
      ...(e.tagName==='A' && e.getAttribute('href')!==null ? {href:e.href} : {})};
    for (const key of ['checked','selected','expanded']) {
      const value=e.getAttribute('aria-'+key);
      if (value!==null) base[key]=value;
    }
    if (['checkbox','radio'].includes(e.type)) base.checked=String(e.checked);
    if (isFile) {
      actions.push({...base,kind:'upload',accept:e.accept||'',multiple:e.multiple});
    } else if (e.tagName==='SELECT') {
      for (const o of e.options) if (!o.selected && !o.disabled && !o.closest('optgroup[disabled]'))
        actions.push({...base,kind:'select',value:o.value,
          current_value:[...e.selectedOptions].map(o=>o.label).join(', '),label:base.label+' → '+o.label});
    } else {
      const editable=!e.readOnly && e.getAttribute('aria-readonly')!=='true' &&
        (['textbox','searchbox','spinbutton'].includes(rname) ||
          (rname==='combobox' && ['INPUT','TEXTAREA'].includes(e.tagName)));
      const value='value' in e ? String(e.value) :
        e.isContentEditable || rname==='combobox' ? e.innerText.trim() : '';
      actions.push({...base,kind:editable?'fill':'click',value,contenteditable:e.isContentEditable});
      if (editable) actions.push({...base,kind:'click',value,label:'Open '+base.label});
    }
  }
  const words=[]; let length=0;
  for (const {root,ox,oy} of textRoots) {
    if (length>=6000) break;
    const walker=document.createTreeWalker(root,NodeFilter.SHOW_TEXT),range=document.createRange();
    let node;
    while ((node=walker.nextNode()) && length<6000) {
      const value=node.textContent.trim(),p=node.parentElement;
      if (!value || !p || closest(p,'script,style,noscript,template') || !visible(p)) continue;
      range.selectNodeContents(node); const r=range.getBoundingClientRect();
      if (r.width>0 && r.height>0 && r.bottom+oy>0 && r.top+oy<innerHeight &&
          r.right+ox>0 && r.left+ox<innerWidth) { words.push(value); length+=value.length; }
    }
  }
  const nested=[];
  for (const e of elements) {
    if (nested.length>=6 || !visible(e) || e.scrollHeight<=e.clientHeight+2) continue;
    const overflow=getComputedStyle(e).overflowY;
    if (!['auto','scroll'].includes(overflow)) continue;
    const r=rect(e),x=r.x+r.w/2,y=r.y+r.h/2;
    if (!r.w || !r.h || x<0 || y<0 || x>=innerWidth || y>=innerHeight) continue;
    const label=name(e)||closest(e,'section,article,[role="region"]')?.getAttribute('aria-label')||'scroll area';
    const node=identity(e);
    nested.push([node,e.scrollTop,e.scrollHeight,e.clientHeight]);
    if (e.scrollTop+e.clientHeight<e.scrollHeight-2)
      actions.push({id:`scroll_${node}_down`,kind:'scroll',node,label:`Scroll down: ${label}`,delta:560,x,y});
    if (e.scrollTop>0)
      actions.push({id:`scroll_${node}_up`,kind:'scroll',node,label:`Scroll up: ${label}`,delta:-560,x,y});
  }
  const active=elements.find(e=>{
    let a=e.ownerDocument.activeElement;
    while (a?.shadowRoot?.activeElement) a=a.shadowRoot.activeElement;
    return a===e;
  });
  if (active && active.matches?.(selector) && role(active) && visible(active)) {
    for (const key of ['Enter','Escape','Tab',' ','Backspace','ArrowUp','ArrowDown','ArrowLeft','ArrowRight'])
      actions.push({kind:'press',node:identity(active),role:role(active)||'control',
        label:`Press ${key===' '?'Space':key} in ${name(active)||role(active)||'focused control'}`,key});
  }
  const text=words.join('\n').slice(0,6000),height=document.documentElement.scrollHeight;
  const page_key=cache.pageKey();
  const elementActions=actions.filter(a=>!a.id);
  const controls=actions.filter(a=>a.id);
  const omitted_actions=Math.max(0,elementActions.length-250);
  elementActions.splice(250);
  elementActions.forEach((a,i)=>a.id='e'+(i+1));
  if (scrollY+innerHeight<height-2) controls.push({id:'scroll_down',kind:'scroll',label:'Scroll down',delta:560,x:550,y:650});
  if (scrollY>0) controls.push({id:'scroll_up',kind:'scroll',label:'Scroll up',delta:-560,x:550,y:130});
  controls.push({id:'wait',kind:'wait',label:'Wait for the page to update'});
  const finalActions=[...elementActions,...controls],guards={};
  for (const a of finalActions) if (a.node && !(a.node in guards)) guards[a.node]=cache.guard(cache.nodes.get(a.node));
  const semantics=finalActions.map(({rect,...action})=>action);
  const marker=[performance.timeOrigin,location.href,scrollX,scrollY,innerWidth,innerHeight,
    document.title,text,semantics,page_key[6],nested];
  // Signals are counts, never content: what kind of page this is, so the
  // deterministic layer can stop before Jev is asked to guess at a login
  // wall, a challenge or a card form.
  const captchaWidgets=captchaFrames+elements.filter(e=>visible(e) &&
    e.matches?.('.g-recaptcha,.h-captcha,.cf-turnstile,[data-sitekey],#px-captcha')).length;
  return {url:location.href,title:document.title,w:innerWidth,h:innerHeight,text,
    scroll:{y:scrollY,height,nested},actions:finalActions,marker,page_key,guards,omitted_actions,
    signals:{cross_origin_frames:crossOriginFrames,captcha:captchaWidgets,
      password_fields:elements.filter(e=>e.type==='password' && visible(e)).length,
      payment_fields:elements.filter(e=>payment(e) && visible(e)).length}};
})()
