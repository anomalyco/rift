#!/usr/bin/env python3
"""Synthetic native CLI E2E. All fixture SQL is limited to newly created temp DBs.
No production database/path defaults exist. The release binary is a generic input.
"""
import argparse, fcntl, hashlib, json, os, pathlib, shutil, sqlite3, subprocess, tempfile, time, traceback

parser = argparse.ArgumentParser()
parser.add_argument('--binary', required=True)
parser.add_argument('--legacy-binary', required=True)
parser.add_argument('--legacy-binary-sha256', required=True)
parser.add_argument('--out', required=True)
parser.add_argument('--production', action='store_true')
parser.add_argument('--expected-source-commit')
args = parser.parse_args()
BINARY = str(pathlib.Path(args.binary).resolve())
LEGACY = str(pathlib.Path(args.legacy_binary).resolve())
OUT = pathlib.Path(args.out).resolve()
OUT.mkdir(parents=True, exist_ok=False)
BASE = pathlib.Path(tempfile.mkdtemp(prefix='rift-guarded-fixture-')).resolve()
RESULTS = []
COMMANDS = []
PROVENANCE=json.loads(subprocess.run([BINARY,'guarded','provenance'],capture_output=True,text=True,check=True).stdout)
if args.expected_source_commit:
    assert PROVENANCE['source_commit']==args.expected_source_commit and not PROVENANCE['source_dirty'], 'source provenance differs'
assert PROVENANCE['fixture_faults'] != args.production, 'incorrect fixture/production binary'
FAULT_SCENARIOS={
    'fault-'+name for name in ('rename_failure','database_failure','before_rename','after_each_rename','after_rename','before_commit','after_commit')
} | {'crash-during-rollback','noncooperative-legacy-writer-stale-recovery','noncooperative-filesystem-stale-recovery','unstable-live-database-copy','rollback-db-commit-failure','crash-after-rollback-commit','journal-publish-failure','finalize-rejects-uncommitted-journal','crash-after-finalize-archive','crash-before-postremove-no-replay-reject-finalize'}

def sha(data): return hashlib.sha256(data).hexdigest()
def file_hash(path): return sha(path.read_bytes()) if path.is_file() else None
assert file_hash(pathlib.Path(LEGACY))==args.legacy_binary_sha256, 'legacy artifact hash differs'

def manifest(path):
    result = {}
    if not path.exists(): return result
    for p in sorted(path.rglob('*')):
        rel = str(p.relative_to(path))
        stat = p.lstat()
        if p.is_symlink(): result[rel] = {'link': os.readlink(p), 'inode': stat.st_ino}
        elif p.is_file(): result[rel] = {'sha256': file_hash(p), 'inode': stat.st_ino, 'mode': stat.st_mode}
        elif p.is_dir(): result[rel] = {'directory': True, 'inode': stat.st_ino}
    return result

def physical(db):
    return {suffix: file_hash(pathlib.Path(str(db)+suffix)) for suffix in ('', '-wal', '-shm')}

def rows(db):
    # Copy live bytes to inspect without touching source SHM or taking source locks.
    with tempfile.TemporaryDirectory(prefix='rift-e2e-read-') as tmp:
        copy = pathlib.Path(tmp)/'copy.sqlite'
        for suffix in ('', '-wal', '-shm'):
            src=pathlib.Path(str(db)+suffix)
            if src.exists(): shutil.copyfile(src, str(copy)+suffix)
        con=sqlite3.connect(copy)
        state={table: con.execute(f'SELECT * FROM {table} ORDER BY id').fetchall() for table in ('rift','trash')}
        state['rowids']=con.execute('SELECT id,rowid FROM rift ORDER BY id').fetchall()
        state['schema']=con.execute('SELECT type,name,sql FROM sqlite_master ORDER BY type,name').fetchall()
        con.close()
        return state

def call(db, *cmd, binary=BINARY, fault=None, expect=0):
    argv=[binary,'--database',str(db),*map(str,cmd)]
    env=os.environ.copy()
    env.pop('RIFT_FIXTURE_FAULT',None)
    if fault: env['RIFT_FIXTURE_FAULT']=fault
    p=subprocess.run(argv,env=env,capture_output=True,text=True,timeout=20,cwd=BASE)
    COMMANDS.append({'argv':argv,'fault':fault,'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr})
    if expect is not None: assert p.returncode==expect,(argv,p.returncode,p.stderr,p.stdout)
    return p

class Fixture:
    def __init__(self,name,legacy=False):
        self.home=BASE/name; self.home.mkdir()
        self.db=self.home/'fixture.sqlite'; self.root=self.home/'root'; self.root.mkdir()
        (self.root/'data.txt').write_text('synthetic source bytes\n')
        subprocess.run(['git','init','-q',str(self.root)],check=True,capture_output=True)
        subprocess.run(['git','-C',str(self.root),'config','user.email','fixture@example.invalid'],check=True)
        subprocess.run(['git','-C',str(self.root),'config','user.name','Synthetic Fixture'],check=True)
        subprocess.run(['git','-C',str(self.root),'add','data.txt'],check=True)
        subprocess.run(['git','-C',str(self.root),'commit','-qm','synthetic fixture'],check=True)
        b=LEGACY if legacy else BINARY
        call(self.db,'init','--here',self.root,binary=b)
        self.child=pathlib.Path(call(self.db,'create',self.root,'--name','child','--into',self.home/'clones','--no-hooks',binary=b).stdout.strip())
        self.sibling=pathlib.Path(call(self.db,'create',self.root,'--name','sibling','--into',self.home/'clones','--no-hooks',binary=b).stdout.strip())
        self.grandchild=pathlib.Path(call(self.db,'create',self.child,'--name','grandchild','--into',self.home/'clones','--no-hooks',binary=b).stdout.strip())
        self.ids={p.name:(p/'.rift').read_text().strip() for p in (self.root,self.child,self.sibling,self.grandchild)}
        self.original=rows(self.db)
        self.original_files={str(p):manifest(p) for p in (self.root,self.child,self.sibling,self.grandchild)}
        self.before_physical=physical(self.db)
    def path_for(self,id): return next(pathlib.Path(r[2]) for r in rows(self.db)['rift'] if r[0]==id)
    def request(self,target='root',hooks=False,closure=None,kind=None):
        id=self.ids[target]
        if closure is None: closure=[self.ids[n] for n in (['root','child','sibling','grandchild'] if target=='root' else ['child','grandchild'] if target=='child' else [target])]
        return {'operation':'trash','kind':kind or ('root' if target=='root' else 'leaf'),'id':id,'closure':closure,'destinations':[{'id':i,'trash':str(self.path_for(i).parent/('.rift-trash-'+i))} for i in closure], 'hooks':hooks}
    def plan(self,request):
        req=self.home/'request.json'; req.write_text(json.dumps(request))
        before=physical(self.db); before_files=manifest(self.home)
        p=call(self.db,'guarded','preflight',req)
        assert before==physical(self.db), 'dry preflight changed DB/WAL/SHM'
        assert before_files==manifest(self.home), 'dry preflight changed fixture files/locks'
        plan=self.home/'plan.json'; plan.write_text(p.stdout)
        return plan
    def apply(self,plan,fault=None,expect=0): return call(self.db,'guarded','apply',plan,fault=fault,expect=expect)
    def journal(self): return pathlib.Path(str(self.db)+'.guarded-journal')
    def rollback(self,expect=0,fault=None,hash=None):
        return call(self.db,'guarded','rollback','--journal-hash',hash or file_hash(self.journal()),expect=expect,fault=fault)
    def restored(self):
        assert rows(self.db)==self.original,'all row fields/schema/history not restored'
        for path,original in self.original_files.items(): assert manifest(pathlib.Path(path))==original,'files/markers/Git changed'
    def receipt(self):
        return {'fixture':str(self.home),'before_db_wal_shm':self.before_physical,'after_db_wal_shm':physical(self.db),'before_state':self.original,'after_state':rows(self.db),'before_files':self.original_files,'after_files':{p:manifest(pathlib.Path(p)) for p in self.original_files},'all_files':manifest(self.home)}

CURRENT=None

def scenario(name,modes,fn):
    global CURRENT
    CURRENT=None
    start=len(COMMANDS)
    skipped=args.production and name in FAULT_SCENARIOS
    try:
        if not skipped: fn()
        result={'scenario':name,'passed':True,'failure_modes':modes}
    except Exception as error:
        result={'scenario':name,'passed':False,'error':str(error),'traceback':traceback.format_exc(),'failure_modes':modes}
    result['skipped']=skipped
    if skipped: result['skip_reason']='explicit fixture-only fault injection'
    if CURRENT: result.update(CURRENT.receipt())
    result['commands']=COMMANDS[start:]
    path=OUT/(name+'.json'); path.write_text(json.dumps(result,indent=2)+'\n')
    RESULTS.append({'scenario':name,'passed':result['passed'],'skipped':skipped,'path':str(path),'sha256':file_hash(path),'failure_modes':modes})
    print(name, 'SKIP' if skipped else 'PASS' if result['passed'] else 'FAIL',flush=True)

def fixture(name,legacy=False):
    global CURRENT
    CURRENT=Fixture(name,legacy); return CURRENT

def success(target='root',legacy=False):
    f=fixture('success-'+target+('-legacy' if legacy else ''),legacy)
    if legacy:
        # True release creates a trash row; preserve it and unknown extra schema.
        old=f.home/'old'; old.mkdir(); (old/'bytes').write_text('old trash synthetic\n')
        call(f.db,'init','--here',old,binary=LEGACY)
        old_child=pathlib.Path(call(f.db,'create',old,'--name','old-child','--into',f.home/'old-clones','--no-hooks',binary=LEGACY).stdout.strip())
        call(f.db,'remove',old_child,'--no-hooks',binary=LEGACY)
        with sqlite3.connect(f.db) as db:
            db.execute('CREATE TABLE legacy_extra (value TEXT)')
            db.execute("INSERT INTO legacy_extra VALUES ('synthetic unchanged')")
        f.original=rows(f.db)
        old_trash=f.original['trash']; assert old_trash,'true 0.0.12 trash fixture missing'
    plan=f.plan(f.request(target)); before=rows(f.db)
    outcome=json.loads(f.apply(plan).stdout); assert outcome['committed'] is True
    request=json.loads(plan.read_text())['request']; selected=request['closure']
    after=rows(f.db)
    assert after['rift']==[r for r in before['rift'] if r[0] not in selected]
    assert after['trash']==before['trash'] and after['schema']==before['schema']
    for d in request['destinations']:
        original=next(pathlib.Path(r[2]) for r in before['rift'] if r[0]==d['id'])
        assert not original.exists() and (pathlib.Path(d['trash'])/'.rift').read_text().strip()==d['id']
    assert call(f.db,'list',f.root,expect=None).returncode!=0,'legacy native writer was not fenced by journal'
    f.rollback(); f.restored()
    if legacy:
        with sqlite3.connect(f.db) as db: assert db.execute('SELECT * FROM legacy_extra').fetchall()==[('synthetic unchanged',)]
        assert call(f.db,'list',f.root,binary=LEGACY).returncode==0

scenario('root-trash-rollback',['rowid_restoration','source_git_mutation','unrelated_ancestry_retired'],lambda:success('root'))
scenario('child-closure-rollback',['protected_ancestry_retired'],lambda:success('child'))
scenario('leaf-trash-rollback',[],lambda:success('sibling'))
scenario('release-0-0-12-compatibility',[],lambda:success('child',True))

def alias():
    f=fixture('alias'); actual=f.home/'actual-root'; f.root.rename(actual); f.root.symlink_to(actual,target_is_directory=True)
    plan=f.plan({'operation':'reconcile','paths':[{'id':f.ids['root'],'canonical':str(actual)}]})
    before=rows(f.db); link=os.readlink(f.root); identity=f.root.lstat().st_ino
    f.apply(plan); after=rows(f.db)
    assert len(before['rift'])==len(after['rift']) and before['trash']==after['trash'] and before['schema']==after['schema']
    for old,new in zip(before['rift'],after['rift']): assert new==tuple(str(actual) if n==2 and old[0]==f.ids['root'] else v for n,v in enumerate(old))
    assert f.root.lstat().st_ino==identity and os.readlink(f.root)==link
    f.rollback(); assert rows(f.db)==before and os.readlink(f.root)==link
    assert manifest(actual)==f.original_files[str(f.root)]
scenario('old-symlink-alias-reconcile',['stored_path_mismatch','canonical_path_mismatch'],alias)

def reject_request(name,mutate):
    f=fixture(name); request=f.request('child'); mutate(f,request)
    p=f.home/'bad-request.json';p.write_text(json.dumps(request)); before=physical(f.db); files=manifest(f.home)
    assert call(f.db,'guarded','preflight',p,expect=None).returncode!=0
    assert before==physical(f.db) and files==manifest(f.home)
    assert rows(f.db)==f.original
scenario('incomplete-child-closure',['closure_incomplete','unexpected_children'],lambda:reject_request('closure',lambda f,r:r['closure'].pop()))
scenario('root-kind-mismatch',['root_kind_mismatch'],lambda:reject_request('rootkind',lambda f,r:r.update(kind='root')))
scenario('leaf-kind-mismatch',['leaf_kind_mismatch'],lambda:reject_request('leafkind',lambda f,r:r.update(id=f.ids['root'])))
scenario('unexpected-id',['unexpected_row_id'],lambda:reject_request('unknownid',lambda f,r:r.update(id='SYNTHETIC-UNKNOWN')))
scenario('occupied-trash',['rename_destination_occupied'],lambda:reject_request('occupied',lambda f,r:pathlib.Path(r['destinations'][0]['trash']).mkdir()))

def sqlite_change(db):
    with sqlite3.connect(db) as con: con.execute("INSERT INTO trash VALUES ('SYNTHETIC-OLD-TRASH','/tmp/synthetic-missing-trash',123)")

def stale(name,mutate):
    f=fixture(name); plan=f.plan(f.request('child')); mutate(f,plan)
    before=physical(f.db); state=rows(f.db); files=manifest(f.home)
    assert f.apply(plan,expect=None).returncode!=0
    assert before==physical(f.db) and files==manifest(f.home) and rows(f.db)==state
    assert not f.journal().exists()
scenario('stale-marker-id',['marker_id_mismatch'],lambda:stale('markerid',lambda f,p:(f.child/'.rift').write_text('SYNTHETIC-WRONG\n')))
scenario('stale-marker-hash',['marker_hash_mismatch'],lambda:stale('markerhash',lambda f,p:(f.child/'.rift').write_text(f.ids['child']+'\n\n')))
scenario('stale-tree-files',['directory_identity_changed'],lambda:stale('treebytes',lambda f,p:(f.child/'data.txt').write_text('changed synthetic bytes')))

def change_plan(f,p,key,value):
    plan=json.loads(p.read_text()); plan['before']['rows'][0][key]=value; p.write_text(json.dumps(plan))
scenario('unexpected-parent',['unexpected_parent'],lambda:stale('parent',lambda f,p:change_plan(f,p,'parent_id','SYNTHETIC-WRONG')))
scenario('stale-db-byte-snapshot',['stale_database_snapshot'],lambda:stale('dbsnapshot',lambda f,p:sqlite_change(f.db)))

def alias_changed():
    f=fixture('aliaschanged'); actual=f.home/'actual'; f.root.rename(actual); f.root.symlink_to(actual,target_is_directory=True)
    plan=f.plan({'operation':'reconcile','paths':[{'id':f.ids['root'],'canonical':str(actual)}]})
    f.root.unlink(); replacement=f.home/'replacement'; shutil.copytree(actual,replacement); f.root.symlink_to(replacement,target_is_directory=True)
    before=physical(f.db); assert f.apply(plan,expect=None).returncode!=0; assert physical(f.db)==before
scenario('alias-identity-change',['alias_identity_changed'],alias_changed)

def hook_case(name,command,post=False):
    f=fixture(name)
    (f.child/'.rift.toml').write_text('version = 1\n[[hooks.'+('postremove' if post else 'preremove')+']]\nrun = '+json.dumps(command)+'\n')
    f.original_files[str(f.child)]=manifest(f.child)
    plan=f.plan(f.request('child',hooks=True))
    p=f.apply(plan,expect=None)
    if post:
        outcome=json.loads(p.stdout); assert outcome['committed'] and outcome['hook_error'] and outcome['status']=='committed_hook_failed'
        assert f.journal().exists() and not f.child.exists(); f.rollback(); f.restored()
    else:
        assert p.returncode!=0 and rows(f.db)==f.original and f.child.exists() and not f.journal().exists()
scenario('preremove-hook-failure',['preremove_failure'],lambda:hook_case('prehook','exit 7'))
scenario('preremove-hook-state-change',['preremove_state_change'],lambda:hook_case('prechange','printf changed >> data.txt'))
scenario('postremove-hook-failure',['postremove_failure_committed'],lambda:hook_case('posthook','exit 9',True))

for fault in ('rename_failure','database_failure','before_rename','after_each_rename','after_rename','before_commit','after_commit'):
    def crash(fault=fault):
        f=fixture('fault-'+fault); plan=f.plan(f.request('root'))
        p=f.apply(plan,fault=fault,expect=None); assert p.returncode!=0 and f.journal().exists()
        outcome=json.loads(call(f.db,'guarded','recover').stdout)
        if fault=='after_commit':
            assert outcome['committed'] and not f.root.exists(); f.rollback()
        else: assert outcome['committed'] is False
        f.restored()
    scenario('fault-'+fault,[{'before_rename':'crash_before_rename','after_each_rename':'rename_partial_failure','after_rename':'crash_after_rename','before_commit':'crash_before_commit','after_commit':'crash_after_commit','database_failure':'database_failure','rename_failure':'rename_failure'}[fault]],crash)

def interrupted_rollback():
    f=fixture('rollbackcrash'); plan=f.plan(f.request('root')); f.apply(plan)
    assert f.rollback(fault='during_rollback',expect=None).returncode==86
    outcome=json.loads(call(f.db,'guarded','recover').stdout); assert not outcome['committed']; f.restored()
scenario('crash-during-rollback',['crash_during_rollback'],interrupted_rollback)

def reject_rollback(name,mutate):
    f=fixture(name); plan=f.plan(f.request('child')); f.apply(plan); mutate(f)
    before=rows(f.db); files=manifest(f.home)
    assert f.rollback(expect=None).returncode!=0
    assert before==rows(f.db) and files==manifest(f.home)
scenario('rollback-occupied-original',['rollback_destination_occupied'],lambda:reject_rollback('rollbackoccupied',lambda f:f.child.mkdir()))
scenario('rollback-stale-registry',['rollback_stale_rows'],lambda:reject_rollback('rollbackstale',lambda f:sqlite_change(f.db)))
scenario('rollback-changed-marker',['rollback_marker_or_files_changed'],lambda:reject_rollback('rollbackmarker',lambda f:(f.child.parent/('.rift-trash-'+f.ids['child'])/'.rift').write_text('SYNTHETIC-WRONG\n')))

def stale_hash():
    f=fixture('stalehash'); f.apply(f.plan(f.request('child'))); before=manifest(f.home)
    assert f.rollback(hash='0'*64,expect=None).returncode!=0; assert before==manifest(f.home)
    f.rollback(); f.restored()
scenario('rollback-stale-journal-hash',['rollback_stale_rows'],stale_hash)

def native_lock():
    f=fixture('nativelock'); plan=f.plan(f.request('child')); before=physical(f.db)
    lock=os.open(f.db.parent,os.O_RDONLY)
    try:
        fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        assert f.apply(plan,expect=None).returncode!=0
        assert call(f.db,'list',f.root,expect=None).returncode!=0
    finally: os.close(lock)
    assert physical(f.db)==before; f.apply(plan); f.rollback(); f.restored()
scenario('native-caller-lock',['concurrent_native_writer'],native_lock)

def db_lock():
    import sys
    f=fixture('dbcontention')
    holder=subprocess.Popen([sys.executable,'-c',"import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('BEGIN IMMEDIATE'); print('locked',flush=True); sys.stdin.readline(); c.rollback()",str(f.db)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
    try:
        assert holder.stdout.readline().strip()=='locked'
        plan=f.plan(f.request('child')); p=f.apply(plan,expect=None)
        assert p.returncode!=0 and not f.journal().exists()
    finally:
        holder.communicate('release\n',timeout=10)
    assert rows(f.db)==f.original

scenario('sqlite-writer-contention',['sqlite_writer_contention'],db_lock)

def journal_gate():
    f=fixture('journalgate'); f.apply(f.plan(f.request('child'))); before=rows(f.db)
    assert call(f.db,'gc',expect=None).returncode!=0
    assert call(f.db,'create',f.root,'--name','forbidden','--into',f.home/'clones','--no-hooks',expect=None).returncode!=0
    assert before==rows(f.db); f.rollback(); f.restored()
scenario('legacy-manager-journal-gate',['legacy_native_writer_during_journal'],journal_gate)

def live_wal():
    f=fixture('livewal'); con=sqlite3.connect(f.db); con.execute('PRAGMA journal_mode=WAL'); con.execute('PRAGMA wal_autocheckpoint=0')
    con.execute("INSERT INTO trash VALUES ('SYNTHETIC-LIVE-WAL','/tmp/synthetic-live-wal',456)"); con.commit()
    assert pathlib.Path(str(f.db)+'-wal').stat().st_size>0
    before=physical(f.db); plan=f.plan(f.request('child')); data=json.loads(plan.read_text())
    assert any(r[0]=='SYNTHETIC-LIVE-WAL' for r in data['before']['trash'])
    assert before==physical(f.db); con.close()
scenario('immutable-live-wal-preflight',['dry_db_wal_shm_mutation','live_wal_ignored'],live_wal)

def reject_plan_field(name,field,value):
    f=fixture(name); plan=f.plan(f.request('child')); data=json.loads(plan.read_text()); data['guards'][0][field]=value; plan.write_text(json.dumps(data))
    before=physical(f.db); files=manifest(f.home)
    assert f.apply(plan,expect=None).returncode!=0 and before==physical(f.db) and files==manifest(f.home)
scenario('stored-path-guard-mismatch',['stored_path_mismatch'],lambda:reject_plan_field('storedpath','stored','/tmp/synthetic-unreviewed'))
scenario('canonical-path-guard-mismatch',['canonical_path_mismatch'],lambda:reject_plan_field('canonicalpath','canonical','/tmp/synthetic-unreviewed'))

def unexpected_child():
    f=fixture('newchild'); plan=f.plan(f.request('child'))
    call(f.db,'create',f.child,'--name','unreviewed-child','--into',f.home/'clones','--no-hooks')
    before=physical(f.db); state=rows(f.db); files=manifest(f.home)
    assert f.apply(plan,expect=None).returncode!=0
    assert physical(f.db)==before and rows(f.db)==state and manifest(f.home)==files
scenario('unreviewed-new-child',['unexpected_children'],unexpected_child)

def protected_nested():
    f=fixture('protectednested'); nested=f.child/'unrelated-sibling'; f.sibling.rename(nested)
    with sqlite3.connect(f.db) as con: con.execute('UPDATE rift SET path=? WHERE id=?',(str(nested),f.ids['sibling']))
    request=f.request('child'); req=f.home/'request.json'; req.write_text(json.dumps(request)); before=physical(f.db); files=manifest(f.home)
    assert call(f.db,'guarded','preflight',req,expect=None).returncode!=0
    assert before==physical(f.db) and files==manifest(f.home)
scenario('protected-unrelated-physical-ancestry',['unrelated_ancestry_retired','protected_ancestry_retired'],protected_nested)

def old_nested_trash():
    f=fixture('oldnestedtrash'); old=f.root/'old-trash'; old.mkdir(); (old/'synthetic').write_text('preserve old trash')
    with sqlite3.connect(f.db) as con: con.execute('INSERT INTO trash VALUES (?,?,?)',('SYNTHETIC-OLD-NESTED',str(old),789))
    request=f.request('root'); req=f.home/'request.json';req.write_text(json.dumps(request)); before=physical(f.db);files=manifest(f.home)
    assert call(f.db,'guarded','preflight',req,expect=None).returncode!=0
    assert before==physical(f.db) and files==manifest(f.home)
scenario('old-trash-objects-protected',['root_old_trash_nested'],old_nested_trash)

def legacy_stale():
    f=fixture('legacystale',True); plan=f.plan(f.request('child'))
    assert f.apply(plan,fault='before_rename',expect=None).returncode==86
    call(f.db,'create',f.root,'--name','legacy-unreviewed','--into',f.home/'clones','--no-hooks',binary=LEGACY)
    before=physical(f.db); state=rows(f.db); files=manifest(f.home)
    assert call(f.db,'guarded','recover',expect=None).returncode!=0
    assert before==physical(f.db) and state==rows(f.db) and files==manifest(f.home)
scenario('noncooperative-legacy-writer-stale-recovery',['legacy_noncooperative_writer_stale','stale_database_snapshot','rollback_stale_rows'],legacy_stale)

def legacy_file_stale():
    f=fixture('legacyfilestale',True); plan=f.plan(f.request('child'))
    assert f.apply(plan,fault='after_rename',expect=None).returncode==86
    trash=f.child.parent/('.rift-trash-'+f.ids['child']); (trash/'data.txt').write_text('outside writer changed files')
    before=physical(f.db); files=manifest(f.home)
    assert call(f.db,'guarded','recover',expect=None).returncode!=0
    assert before==physical(f.db) and files==manifest(f.home)
scenario('noncooperative-filesystem-stale-recovery',['rollback_marker_or_files_changed'],legacy_file_stale)

def unstable_snapshot():
    import sys
    f=fixture('unstablesnapshot'); request=f.request('child'); req=f.home/'request.json';req.write_text(json.dumps(request))
    writer=subprocess.Popen([sys.executable,'-c',"import sqlite3,sys,time; time.sleep(.15); c=sqlite3.connect(sys.argv[1]); c.execute(\"INSERT INTO trash VALUES ('SYNTHETIC-RACE','/tmp/synthetic-race',901)\"); c.commit()",str(f.db)])
    try:
        p=call(f.db,'guarded','preflight',req,fault='snapshot_pause',expect=None)
        assert p.returncode!=0 and 'changed during dry snapshot' in p.stderr
    finally: writer.wait(timeout=10)
    assert f.child.exists() and not f.journal().exists()
scenario('unstable-live-database-copy',['unstable_database_copy'],unstable_snapshot)


def rollback_fault(fault):
    f=fixture('rollbackfault-'+fault); f.apply(f.plan(f.request('root')))
    assert f.rollback(fault=fault,expect=None).returncode!=0
    outcome=json.loads(call(f.db,'guarded','recover').stdout)
    assert not outcome['committed']; f.restored()
scenario('rollback-db-commit-failure',['rollback_commit_failure'],lambda:rollback_fault('rollback_commit_failure'))
scenario('crash-after-rollback-commit',['crash_after_rollback_commit'],lambda:rollback_fault('after_rollback_commit'))

def journal_publish():
    f=fixture('journalpublish');plan=f.plan(f.request('root'))
    assert f.apply(plan,fault='journal_publish_failure',expect=None).returncode!=0 and not f.journal().exists()
    f.restored()
scenario('journal-publish-failure',['journal_publish_failure'],journal_publish)

def post_change():
    f=fixture('postchange');(f.child/'.rift.toml').write_text('version=1\n[[hooks.postremove]]\nrun="printf changed >> data.txt"\n')
    plan=f.plan(f.request('child',hooks=True));p=f.apply(plan,expect=None);outcome=json.loads(p.stdout)
    assert p.returncode!=0 and outcome['committed'] and outcome['status']=='committed_hook_failed'
    before=physical(f.db);files=manifest(f.home)
    assert f.rollback(expect=None).returncode!=0 and physical(f.db)==before and manifest(f.home)==files
scenario('postremove-state-change-committed',['postremove_state_change'],post_change)

def unknown_stale():
    f=fixture('unknownstale')
    with sqlite3.connect(f.db) as con:
        con.execute('CREATE TABLE old_unknown (value BLOB)');con.execute('INSERT INTO old_unknown VALUES (?)',(b'synthetic-old',))
    f.apply(f.plan(f.request('child')))
    with sqlite3.connect(f.db) as con:con.execute('UPDATE old_unknown SET value=?',(b'synthetic-unreviewed',))
    before=physical(f.db);files=manifest(f.home)
    assert f.rollback(expect=None).returncode!=0 and before==physical(f.db) and files==manifest(f.home)
scenario('unknown-table-stale-rollback',['unknown_tables_stale'],unknown_stale)

def ancestor_alias():
    f=fixture('ancestoralias'); actual_container=f.home/'actual-container';actual_container.mkdir(); actual=actual_container/'root'; f.root.rename(actual)
    old_container=f.home/'old-container'; old_container.symlink_to(actual_container,target_is_directory=True); stored=old_container/'root'
    with sqlite3.connect(f.db) as con:con.execute('UPDATE rift SET path=? WHERE id=?',(str(stored),f.ids['root']))
    plan=f.plan({'operation':'reconcile','paths':[{'id':f.ids['root'],'canonical':str(actual)}]})
    old_container.unlink();old_container.symlink_to(actual_container,target_is_directory=True)
    before=physical(f.db);files=manifest(f.home)
    assert f.apply(plan,expect=None).returncode!=0 and physical(f.db)==before and manifest(f.home)==files
scenario('ancestor-alias-identity-changed',['alias_ancestor_identity_changed'],ancestor_alias)


def finalize(f,hash=None,expect=0,fault=None):
    return call(f.db,'guarded','finalize','--journal-hash',hash or file_hash(f.journal()),expect=expect,fault=fault)
def history_rollback(f,path,expect=0):
    return call(f.db,'guarded','rollback','--journal-hash',file_hash(path),'--history',path,expect=expect)

def sequential():
    f=fixture('sequential'); actual=f.home/'actual-root'; f.root.rename(actual); f.root.symlink_to(actual,target_is_directory=True)
    original=rows(f.db);alias=os.readlink(f.root)
    f.apply(f.plan({'operation':'reconcile','paths':[{'id':f.ids['root'],'canonical':str(actual)}]}))
    history_a=pathlib.Path(json.loads(finalize(f).stdout)['journal']);assert history_a.exists() and not f.journal().exists()
    after_reconcile=rows(f.db)
    call(f.db,'list',actual)
    f.apply(f.plan(f.request('child')))
    history_b=pathlib.Path(json.loads(finalize(f).stdout)['journal']);assert history_b.exists() and not f.journal().exists()
    call(f.db,'list',actual)
    assert os.readlink(f.root)==alias and history_a.exists()
    history_rollback(f,history_b);assert rows(f.db)==after_reconcile
    history_rollback(f,history_a);assert rows(f.db)==original
    assert history_a.exists() and history_b.exists() and os.readlink(f.root)==alias
    assert manifest(actual)==f.original_files[str(f.root)]
scenario('reconcile-finalize-trash-finalize-retained-caller-history-rollback',[],sequential)

def completed_root():
    f=fixture('completedroot');retained=f.home/'retained-root';retained.mkdir();(retained/'synthetic.txt').write_text('retained unrelated bytes')
    call(f.db,'init','--here',retained);f.original=rows(f.db);retained_files=manifest(retained)
    f.apply(f.plan(f.request('root')));history=pathlib.Path(json.loads(finalize(f).stdout)['journal'])
    call(f.db,'list',retained);assert manifest(retained)==retained_files
    history_rollback(f,history);f.restored();assert manifest(retained)==retained_files
scenario('root-finalize-retained-native-caller-exact-history-rollback',[],completed_root)

def finalized_stale():
    f=fixture('historystale');f.apply(f.plan(f.request('child')));history=pathlib.Path(json.loads(finalize(f).stdout)['journal'])
    call(f.db,'create',f.root,'--name','retained-new-child','--into',f.home/'clones','--no-hooks')
    before=physical(f.db);files=manifest(f.home)
    assert history_rollback(f,history,expect=None).returncode!=0 and before==physical(f.db) and files==manifest(f.home)
    assert not f.journal().exists() and history.exists()
scenario('history-rollback-rejects-fresh-unrelated-native-state',['history_rollback_stale_state'],finalized_stale)

def reject_finalize(name,mutate,fault=None):
    f=fixture(name);f.apply(f.plan(f.request('child')),fault=fault,expect=None);mutate(f)
    before=physical(f.db);files=manifest(f.home)
    assert finalize(f,expect=None).returncode!=0 and before==physical(f.db) and files==manifest(f.home)
scenario('finalize-rejects-uncommitted-journal',['finalize_uncommitted'],lambda:reject_finalize('finalizepending',lambda f:None,'before_rename'))
scenario('finalize-rejects-stale-registry',['finalize_stale_registry'],lambda:reject_finalize('finalizestale',lambda f:sqlite_change(f.db)))
scenario('finalize-rejects-changed-files',['finalize_changed_files'],lambda:reject_finalize('finalizefiles',lambda f:(f.child.parent/('.rift-trash-'+f.ids['child'])/'data.txt').write_text('outside change')))

def finalize_hash():
    f=fixture('finalizehash');f.apply(f.plan(f.request('child')));before=manifest(f.home)
    assert finalize(f,hash='0'*64,expect=None).returncode!=0 and before==manifest(f.home)
    history=pathlib.Path(json.loads(finalize(f).stdout)['journal']);wrong=f.home/'wrong-history';wrong.write_bytes(history.read_bytes());before=manifest(f.home)
    assert history_rollback(f,wrong,expect=None).returncode!=0 and before==manifest(f.home)
scenario('finalize-and-history-hash-path-guards',['finalize_hash_mismatch','history_rollback_wrong_path'],finalize_hash)

def finalize_crash():
    f=fixture('finalizecrash');f.apply(f.plan(f.request('child')));original_hash=file_hash(f.journal())
    assert finalize(f,fault='after_finalize_archive',expect=None).returncode==86
    history=pathlib.Path(str(f.db)+'.guarded-history-'+original_hash)
    assert not f.journal().exists() and history.exists()
    call(f.db,'list',f.root);history_rollback(f,history);f.restored()
scenario('crash-after-finalize-archive',['crash_after_finalize_archive'],finalize_crash)


def hook_crash():
    f=fixture('hookcrash');(f.child/'.rift.toml').write_text('version=1\n[[hooks.postremove]]\nrun="printf fired > hook-fired.txt"\n')
    f.original_files[str(f.child)]=manifest(f.child)
    assert f.apply(f.plan(f.request('child',hooks=True)),fault='after_commit',expect=None).returncode==86
    outcome=json.loads(call(f.db,'guarded','recover').stdout)
    assert outcome['committed'] and outcome['hook_error'] and outcome['status']=='committed_hooks_unconfirmed'
    trash=f.child.parent/('.rift-trash-'+f.ids['child']);assert not (trash/'hook-fired.txt').exists()
    before=physical(f.db);files=manifest(f.home)
    assert finalize(f,expect=None).returncode!=0 and before==physical(f.db) and files==manifest(f.home)
    f.rollback();f.restored()
scenario('crash-before-postremove-no-replay-reject-finalize',['crash_before_postremove','postremove_unconfirmed_finalize'],hook_crash)

def replace_directory(f,p):
    backup=f.home/'synthetic-original-child';f.child.rename(backup);shutil.copytree(backup,f.child)
scenario('changed-directory-inode',['directory_identity_changed'],lambda:stale('directoryinode',replace_directory))

provenance=json.loads(subprocess.run([BINARY,'guarded','provenance'],capture_output=True,text=True,check=True).stdout)
receipt={'provenance':provenance,'production':args.production,'executed_scenarios':sum(not r['skipped'] for r in RESULTS),'version':1,'synthetic_only':True,'fixture_root':str(BASE),'binary':{'path':BINARY,'sha256':file_hash(pathlib.Path(BINARY))},'legacy_binary':{'path':LEGACY,'sha256':file_hash(pathlib.Path(LEGACY)),'release':'0.0.12','source_commit':'6b15e6a4dca5e95362324d5bd1c20134034d2aea'},'results':RESULTS,'passed':all(r['passed'] for r in RESULTS),'failure_modes_file_sha256':file_hash(pathlib.Path(__file__).with_name('guarded-failure-modes.json'))}
(OUT/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
print(json.dumps({'passed':receipt['passed'],'scenarios':len(RESULTS),'receipt':str(OUT/'receipt.json')}),flush=True)
raise SystemExit(0 if receipt['passed'] else 1)
