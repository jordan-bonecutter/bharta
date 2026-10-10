#!/usr/bin/env python3
"""Isolated egui/Sway UI harness. Never connects input to the desktop session.
Build first: cargo build --release --examples && cargo build --release
Run: python3 tests/headless_ui.py /tmp/bharta-headless
Add --audio-only to check audio controls, event updates, and drawer height.
Requires sway, grim, dbus-daemon, and Python 3. Screenshots/logs go to the argument.
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
from PIL import Image, ImageChops

ROOT = Path(__file__).resolve().parents[1]
BAR = Path(os.environ.get("BHARTA_BINARY", ROOT / "target/release/bharta"))
DEST = Path(sys.argv[1] if len(sys.argv) > 1 else '/tmp/bharta-headless').resolve()
DEST.mkdir(parents=True, exist_ok=True)
processes = []
with tempfile.TemporaryDirectory(prefix='bharta-headless-') as directory:
    runtime = Path(directory)
    runtime.chmod(0o700)
    env = os.environ.copy()
    for key in ['SWAYSOCK', 'WAYLAND_DISPLAY', 'DISPLAY', 'DBUS_SESSION_BUS_ADDRESS']:
        env.pop(key, None)
    env.update(XDG_CONFIG_HOME=str(runtime/'settings'), XDG_RUNTIME_DIR=str(runtime), WLR_BACKENDS='headless', BHARTA_HEADLESS='1',
               WLR_RENDERER='pixman', WLR_HEADLESS_OUTPUTS='1',
               DBUS_SYSTEM_BUS_ADDRESS='unix:path=' + str(runtime / 'no-system-bus'),
               PULSE_SERVER='unix:' + str(runtime / 'no-audio-server'))
    state = runtime / 'volume.json'
    state.write_text('[32768, 32768]')
    routing = runtime / 'routing.json'
    routing.write_text(json.dumps({'default':'test', 'test':'speaker', 'hdmi':'hdmi'}))
    moves = runtime / 'moves.json'
    moves.write_text('[]')
    fake = runtime / 'pactl'
    fake.write_text('''#!/usr/bin/env python3
import json,sys,os
from pathlib import Path
p=Path(os.environ['BHARTA_TEST_AUDIO'])
r=Path(os.environ['BHARTA_TEST_ROUTING'])
route=json.loads(r.read_text())
a=sys.argv[1:]
if a==['subscribe']:
 import time
 previous=None
 while True:
  pidfile=Path(os.environ['BHARTA_TEST_WINDOW_PID'])
  current=(p.read_text(),r.read_text(),pidfile.exists())
  if current!=previous:
   print("Event 'change' on sink #0",flush=True);previous=current
  time.sleep(.01)
elif a==['get-default-sink']:print(route['default'])
elif a==['--format=json','list','sinks']:
 v=json.loads(p.read_text())
 outputs=[]
 for name,description,ports in [('test','Built-in audio',[('speaker','Speakers'),('headphones','Headphones')]),('hdmi','External display',[('hdmi','HDMI')])]:
  outputs.append({'name':name,'description':description,'channel_map':'front-left,front-right','volume':{k:{'value':n} for k,n in zip(['front-left','front-right'],v)},'ports':[{'name':n,'description':d,'availability':'yes'} for n,d in ports],'active_port':route[name],'mute':False})
 print(json.dumps(outputs))
elif a==['--format=json','list','sink-inputs']:
 streams=[
  {'index':11,'corked':False,'mute':False,'volume':{'front-left':{'value':49152},'front-right':{'value':49152}},'properties':{'application.name':'Firefox','media.name':'YouTube'}},
  {'index':12,'corked':False,'mute':False,'volume':{'front-left':{'value':32768},'front-right':{'value':32768}},'properties':{'application.name':'Spotify','media.name':'Music'}},
 ]
 pidfile=os.environ.get('BHARTA_TEST_WINDOW_PID')
 if pidfile and Path(pidfile).exists():
  streams.append({'index':13,'corked':False,'mute':False,'volume':{'front-left':{'value':32768},'front-right':{'value':32768}},'properties':{'application.name':'Fixture','application.process.id':Path(pidfile).read_text().strip()}})
 print(json.dumps(streams))
elif a and a[0]=='set-sink-volume':p.write_text(json.dumps([int(n) for n in a[2:]]))
elif a and a[0]=='set-sink-port':route[a[1]]=a[2];r.write_text(json.dumps(route))
elif a and a[0]=='set-default-sink':route['default']=a[1];r.write_text(json.dumps(route))
elif a and a[0]=='move-sink-input':
 m=Path(os.environ['BHARTA_TEST_MOVES']);values=json.loads(m.read_text());values.append(a[1:]);m.write_text(json.dumps(values))
elif 'list' in a:print('[]')
''')
    fake.chmod(0o755)
    fake_capture = runtime / 'parec'
    fake_capture.write_text('''#!/usr/bin/env python3
import math,struct,sys,time
phase=0.0
rate=24000
args=sys.argv[1:]
frequency=750 if '--monitor-stream=11' in args else 1600
while True:
 values=[]
 for _ in range(1024):
  values.append(int(10000*math.sin(phase)))
  phase += 2*math.pi*frequency/rate
  if phase > 2*math.pi: phase -= 2*math.pi
 sys.stdout.buffer.write(struct.pack('<1024h',*values));sys.stdout.buffer.flush()
 time.sleep(1024/rate)
''')
    fake_capture.chmod(0o755)
    env['PATH'] = str(runtime) + os.pathsep + env['PATH']
    env['BHARTA_TEST_AUDIO'] = str(state)
    env['BHARTA_TEST_ROUTING'] = str(routing)
    env['BHARTA_TEST_MOVES'] = str(moves)
    initial_settings=Path(env['XDG_CONFIG_HOME'])/'bharta'
    initial_settings.mkdir(parents=True)
    (initial_settings/'config.json').write_text(json.dumps({'intervals':{'volume_ms':10000}}))
    env['BHARTA_TEST_WINDOW_PID'] = str(runtime / 'fixture.pid')
    env['BHARTA_TEST_ANIMATE'] = str(runtime / 'animate')
    for app, color in [('FIREFOX', (220, 105, 40)), ('SPOTIFY', (40, 180, 90))]:
        art = runtime / (app.lower() + '.png')
        Image.new('RGB', (32, 32), color).save(art)
        env['BHARTA_TEST_' + app + '_ART'] = art.as_uri()
    config = runtime / 'config'
    config.write_text('output HEADLESS-1 mode 1600x900\nseat seat0 fallback true\n')
    def start(args, log):
        p = subprocess.Popen(args, cwd=ROOT, env=env, stdout=open(DEST / log, 'w'),
                             stderr=subprocess.STDOUT, start_new_session=True)
        processes.append(p)
        return p
    def run(args):
        return subprocess.run(args, cwd=ROOT, env=env, check=True, capture_output=True, text=True)
    def probe(x, y, *args, output='HEADLESS-1', size=(1600,900)):
        return start([str(ROOT / 'target/release/examples/ui_probe'), output,
                      str(x), str(y), *map(str,size), *map(str, args)], 'input.log')
    def shot(name):
        run(['grim', '-o', 'HEADLESS-1', str(DEST / (name + '.png'))])
    try:
        bus_config=runtime / 'bus.conf'
        bus_config.write_text('<busconfig><type>session</type><listen>unix:tmpdir=' + str(runtime) + '</listen><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
        bus = run(['dbus-daemon', '--config-file=' + str(bus_config), '--fork', '--print-address=1', '--print-pid=1']).stdout.splitlines()
        env['DBUS_SESSION_BUS_ADDRESS'] = bus[0]
        sway = start(['sway', '--unsupported-gpu', '-c', str(config)], 'sway.log')
        for _ in range(100):
            sockets = list(runtime.glob('sway-ipc.*.sock'))
            displays = [p for p in runtime.glob('wayland-*') if not p.name.endswith('.lock')]
            if sockets and displays:
                break
            if sway.poll() is not None:
                raise RuntimeError('Headless Sway exited; see sway.log')
            time.sleep(.1)
        assert sockets and displays, 'Headless compositor did not become ready'
        env['SWAYSOCK'] = str(sockets[0])
        env['WAYLAND_DISPLAY'] = displays[0].name
        assert Path(env['SWAYSOCK']).parent == runtime
        # Seed a keyboard before mapping layer surfaces: a headless seat has no
        # physical keyboard to receive the initial Wayland focus event.
        keyboard=probe(600,600,'--keyboard-hold',120000);time.sleep(.8)
        bar = start([str(BAR), '--dark', '--output', 'HEADLESS-1'], 'bar.log')
        for _ in range(100):
            assert bar.poll() is None, 'egui bar exited before mapping'
            shot('bar')
            if Image.open(DEST / 'bar.png').getpixel((800,10))[:3] != (0,0,0):
                break
            time.sleep(.2)
        else:
            raise AssertionError('egui bar did not map; see bar.log')
        # The installed font changes the clock width. Locate the battery's
        # fixed-width outline, then use the adjacent Wi-Fi slot.
        bar_pixels=Image.open(DEST/'bar.png').convert('RGB')
        battery_left=next((x for x in range(1000,1500)
            if all(min(bar_pixels.getpixel((dx,9)))>100 for dx in range(x,x+16))),None)
        assert battery_left is not None, 'Cannot locate battery/Wi-Fi controls'
        wifi_x=battery_left-24
        sound_x=battery_left-64
        cpu_x=battery_left-124
        search_fixture=start(['python3','-c',
            'import ctypes,time; ctypes.CDLL(None).prctl(15,b"qzprocess",0,0,0); time.sleep(120)'], 'cpu-search-fixture.log')
        time.sleep(2)
        def cpu_ticks(pid):
            fields=Path(f'/proc/{pid}/stat').read_text().split()
            return int(fields[13])+int(fields[14])
        started=time.monotonic();previous=cpu_ticks(bar.pid)
        time.sleep(3)
        idle_cpu=(cpu_ticks(bar.pid)-previous)/os.sysconf('SC_CLK_TCK')/(time.monotonic()-started)*100
        (DEST/'idle-cpu.txt').write_text(f'{idle_cpu:.2f}% CPU\n')
        assert idle_cpu<15, f'Idle bar continually redraws: {idle_cpu:.1f}% CPU'
        cpu_hover=probe(cpu_x,14,'--hover',5000)
        monitor_left=max(8,min(cpu_x-160,1600-328))
        deadline=time.monotonic()+4
        while time.monotonic()<deadline:
            time.sleep(.2);shot('process-monitor')
            monitor=Image.open(DEST/'process-monitor.png').convert('RGB')
            text_pixels=sum(1 for y in range(90,180) for x in range(int(monitor_left)+12,int(monitor_left)+155)
                            if min(monitor.getpixel((x,y)))>100)
            if text_pixels>20:break
        assert text_pixels>20, 'CPU monitor has no process rows'
        cpu_hover.wait()
        # CPU search takes focus on hover and finds quiet processes.
        # Typing stays on the private headless keyboard.
        typed=probe(cpu_x,14,'--keys-only',16,44,25,19,24,46,18,31,31);typed.wait()
        time.sleep(.4);shot('process-search-name')
        filtered=Image.open(DEST/'process-search-name.png').convert('RGB')
        assert filtered.getpixel((int(monitor_left)+20,120))!=(0,0,0), 'Process name search lost its result row'
        assert filtered.getpixel((int(monitor_left)+20,210))==(0,0,0), 'Process name search did not filter rows'
        name_bounds=(int(monitor_left)+12,110,int(monitor_left)+118,138)
        name_result=filtered.crop(name_bounds)
        digit_keys={'0':11,'1':2,'2':3,'3':4,'4':5,'5':6,'6':7,'7':8,'8':9,'9':10}
        typed=probe(cpu_x,14,'--keys-only',*[14]*9,*[digit_keys[n] for n in str(search_fixture.pid)]);typed.wait()
        time.sleep(.4);shot('process-search-pid')
        filtered=Image.open(DEST/'process-search-pid.png').convert('RGB')
        assert filtered.getpixel((int(monitor_left)+20,120))!=(0,0,0), 'PID search lost its result row'
        assert filtered.getpixel((int(monitor_left)+20,210))==(0,0,0), 'PID search did not filter rows'
        assert ImageChops.difference(name_result,filtered.crop(name_bounds)).getbbox() is None, 'Name and PID searches found different processes'
        typed=probe(cpu_x,14,'--keys-only',*[14]*len(str(search_fixture.pid)),44,44,44,44);typed.wait()
        time.sleep(.4);shot('process-search-empty')
        assert Image.open(DEST/'process-search-empty.png').convert('RGB').getpixel((int(monitor_left)+20,210))==(0,0,0), 'Unmatched process search retained rows'
        assert ImageChops.difference(name_result,Image.open(DEST/'process-search-empty.png').convert('RGB').crop(name_bounds)).getbbox(), 'Unmatched search still shows the matching process'
        typed=probe(cpu_x,14,'--keys-only',14,14,14,14);typed.wait()
        leave_cpu=probe(600,600,'--hover',900);leave_cpu.wait()
        cpu_clicked=probe(cpu_x,14,'--click-hold',1000);time.sleep(.6);shot('process-monitor-clicked');cpu_clicked.wait()
        assert Image.open(DEST/'process-monitor-clicked.png').convert('RGB').getpixel((int(monitor_left)+5,80))!=(0,0,0), 'Clicking CPU did not open its monitor'
        leave_cpu=probe(600,600,'--hover',900);leave_cpu.wait()
        # All input is scoped to HEADLESS-1 on the private Wayland socket.
        holder = probe(sound_x, 14, '--click-hold', 6000)
        time.sleep(1)
        shot('sound')
        assert Image.open(DEST / 'sound.png').getpixel((1250,80))[:3] != (0,0,0), 'Sound popup missing'
        sound_image=Image.open(DEST / 'sound.png').convert('RGB')
        assert sound_image.crop((1180,180,1530,520)).getbbox(), 'Per-application audio controls missing'
        pixels=Image.open(DEST / 'sound.png').convert('RGB')
        # Find the native scale's long, bright filled trough, independent of
        # typography and spacing tweaks.
        runs=[]
        for y in range(75,110):
            begin=None
            for x in range(1100,1550):
                bright=min(pixels.getpixel((x,y)))>150
                if bright and begin is None: begin=x
                if not bright and begin is not None:
                    runs.append((x-begin,begin,y));begin=None
        length,left,slider_y=max(runs)
        assert length>60, 'Native volume scale not found'
        drag=probe(left+length-5,slider_y,'--drag',left+45,slider_y)
        deadline=time.monotonic()+2.5
        while time.monotonic()<deadline:
            time.sleep(.05)
            during=json.loads(state.read_text())
            if during[0]<32768:break
        assert during[0] < 32768, f'Audio did not change while dragging: {during}'
        assert drag.poll() is None, 'Slider updated only after releasing the drag'
        drag.wait();time.sleep(.8)
        final=json.loads(state.read_text())
        assert 0 < final[0] <= during[0] < 32768, 'Final slider position was lost'
        time.sleep(.3)
        assert json.loads(state.read_text())==final, 'Volume changed after commands settled'
        # All destinations are visible without scrolling, with a reserved
        # selection column. Clicking a port on another sink also moves playback.
        popup_left=max(8,min(sound_x-168,1600-344))
        marker_x=int(popup_left)+20
        marker_ys=[y for y in range(280,600)
                   if min(pixels.getpixel((marker_x,y)))>100
                   and max(pixels.getpixel((marker_x-5,y)))<100
                   and max(pixels.getpixel((marker_x+5,y)))<100]
        assert marker_ys, 'Output table selection marker missing or clipped'
        speaker_y=min(marker_ys)+2
        reopen=probe(sound_x,14,'--hover',800);reopen.wait()
        keeper=probe(marker_x+60,speaker_y,'--hover',2500)
        time.sleep(.2);shot('external-volume-before')
        state.write_text('[61440, 61440]')
        time.sleep(.6);shot('external-volume-after')
        before=Image.open(DEST/'external-volume-before.png').convert('RGB')
        after=Image.open(DEST/'external-volume-after.png').convert('RGB')
        slider_rect=(int(popup_left)+12,slider_y-7,int(popup_left)+324,slider_y+8)
        assert ImageChops.difference(before,after).crop(slider_rect).getbbox(), 'External volume change waited for polling'
        state.write_text(json.dumps(final));keeper.wait()
        for offset,sink,port in [(29,'test','headphones'),(58,'hdmi','hdmi'),(0,'test','speaker')]:
            ready=runtime/'output-ready'
            ready.unlink(missing_ok=True)
            click=probe(sound_x,14,'--hover-click',marker_x+60,speaker_y+offset,ready)
            time.sleep(.7);ready.write_text('ready')
            click.wait()
            deadline=time.monotonic()+2
            while time.monotonic()<deadline:
                selected=json.loads(routing.read_text())
                if selected['default']==sink and selected[sink]==port:break
                time.sleep(.05)
            assert selected['default']==sink and selected[sink]==port, f'Destination did not switch: {selected}'
            while time.monotonic()<deadline and ['11',sink] not in json.loads(moves.read_text()):time.sleep(.05)
            assert ['11',sink] in json.loads(moves.read_text()), 'Existing playback did not follow the output'
        reopen=probe(sound_x,14,'--hover',800);time.sleep(.6);shot('output-table');reopen.wait()
        hover=probe(1580,14,'--hover',4000)
        time.sleep(3)
        shot('bar-hover')
        assert Image.open(DEST / 'bar-hover.png').getpixel((1250,80))[:3] != (0,0,0), 'Popup closed over the bar'
        hover.wait();holder.wait()
        if '--audio-only' in sys.argv[2:]:
            bar.terminate();bar.wait()
            (initial_settings/'config.json').write_text(json.dumps({
                'layout':{'sound_max_height':200},'intervals':{'volume_ms':10000},
            }))
            capped=start([str(BAR),'--dark','--output','HEADLESS-1'],'capped-sound.log')
            time.sleep(1.2)
            capped_hover=probe(sound_x,14,'--hover',1800);time.sleep(.8);shot('capped-sound')
            capped_image=Image.open(DEST/'capped-sound.png').convert('RGB')
            drawer_bounds=capped_image.crop((int(popup_left),28,int(popup_left)+336,900)).getbbox()
            assert drawer_bounds and 220<=drawer_bounds[3]<=224, f'Configured Sound height not applied: {drawer_bounds}'
            capped_hover.wait()
            assert capped.poll() is None, 'Capped Sound drawer exited'
            print('Headless audio screenshots and logs:', DEST)
            sys.exit(0)
        outside=probe(600,600,'--hover',4000)
        time.sleep(3)
        shot('dismissed')
        assert Image.open(DEST / 'dismissed.png').getpixel((1250,80))[:3] == (0,0,0), 'Popup did not dismiss'
        outside.wait()
        apps=probe(72,14,'--hover',2500);time.sleep(1);shot('launcher');apps.wait()
        typed=probe(72,14,'--keys-only',44,44,44,44);typed.wait();shot('autofocus-search')
        assert Image.open(DEST / 'autofocus-search.png').convert('RGB').crop((0,200,500,900)).getbbox() is None, 'Typing without clicking search did not filter Apps'
        escape=probe(72,14,'--keys-only',1);escape.wait();time.sleep(.5);shot('escape')
        assert Image.open(DEST / 'escape.png').convert('RGB').crop((0,28,1600,900)).getbbox() is None, 'Escape did not close launcher'
        fixture=start([str(ROOT / 'target/release/examples/headless_window')],'windows.log');time.sleep(2);shot('fixture')
        bar_image=Image.open(DEST / 'fixture.png').convert('RGB')
        green=sum(1 for y in range(28) for x in range(95,170)
                  if bar_image.getpixel((x,y))[1] > bar_image.getpixel((x,y))[0]*1.25)
        assert green >= 4, 'Audible workspace lacks a visible sound indicator'
        preview=probe(120,14,'--hover',2500);time.sleep(1);shot('workspace');preview.wait()
        # The preview stays bounded even with full-sized captured windows.
        # Its rightmost edge must remain within the small left-side popup.
        image=Image.open(DEST / 'workspace.png').convert('RGB')
        baseline=Image.open(DEST / 'fixture.png').convert('RGB')
        bounds=ImageChops.difference(image,baseline).crop((0,28,1600,900)).getbbox()
        assert bounds and 400<=bounds[2]-bounds[0]<=450 and 150<=bounds[3]-bounds[1]<=330, f'Preview does not fit its display bounds: {bounds}'
        # Server decorations are absent from toplevel captures. The preview
        # must draw their title rows instead of stretching the app over them.
        def title_colors(name):
            pixels=Image.open(DEST/f'{name}.png').convert('RGB').crop((20,64,420,280))
            counts={color:count for count,color in pixels.getcolors(pixels.width*pixels.height)}
            return {color:counts.get(color,0) for color in [(40,85,119),(34,34,34)]}
        colors=title_colors('workspace')
        assert all(count>100 for count in colors.values()), f'Missing tiled title bars: {colors}'
        for layout in ['tabbed','stacking']:
            run(['swaymsg','layout '+layout]);time.sleep(.8)
            titles=probe(120,14,'--hover',1200);time.sleep(.7);shot('decorations-'+layout);titles.wait()
            colors=title_colors('decorations-'+layout)
            assert all(count>100 for count in colors.values()), f'Missing {layout} title bars: {colors}'
        run(['swaymsg','layout splith']);time.sleep(.8)
        def preview_title(name):
            return Image.open(DEST/f'{name}.png').convert('RGB').crop((20,40,130,56)).tobytes()
        title_one=preview_title('workspace')
        run(['swaymsg','[app_id="bharta-fixture" title="second window"] move container to workspace 2'])
        time.sleep(1.2)
        second_workspace=probe(160,14,'--hover',1000);time.sleep(.7);shot('workspace-two');second_workspace.wait()
        title_two=preview_title('workspace-two')
        assert title_two!=title_one, 'Workspace 2 preview did not open'
        for name,x,expected in [('fast-one',120,title_one),('fast-two',160,title_two),('fast-return',120,title_one)]:
            fast=probe(x,14,'--hover',250);time.sleep(.08);shot(name)
            assert preview_title(name)==expected, 'Workspace name did not change immediately'
            time.sleep(.4);shot(name+'-settled')
            assert preview_title(name+'-settled')==expected, 'Workspace fade did not reach the latest preview'
            transitioning=Image.open(DEST/f'{name}.png').convert('RGB')
            settled=Image.open(DEST/f'{name}-settled.png').convert('RGB')
            assert transitioning.getpixel((10,60))==settled.getpixel((10,60)), 'Workspace backing faded during switching'
            assert transitioning.crop((20,64,420,280)).tobytes()!=settled.crop((20,64,420,280)).tobytes(), 'Workspace preview skipped the requested fade'
            backing=settled.getpixel((10,60))
            popup_bottom=max(y for y in range(60,400) if settled.getpixel((10,y))==backing)
            assert 280<=popup_bottom<=305, 'Workspace preview has excess bottom space'
            fast.wait()
        click_ready=runtime/'preview-click-ready'
        target=probe(160,14,'--hover-click',100,100,click_ready)
        for _ in range(30):
            time.sleep(.1);shot('click-preview-ready')
            if preview_title('click-preview-ready')==title_two:break
        else:raise AssertionError('Workspace 2 never became ready for clicking')
        click_ready.touch();target.wait()
        workspaces=json.loads(run(['swaymsg','-t','get_workspaces']).stdout)
        (DEST/'preview-click-workspaces.json').write_text(json.dumps(workspaces))
        assert any(w['name']=='2' and w['focused'] for w in workspaces), 'Clicking preview did not switch to its workspace'
        run(['swaymsg','workspace 1']);time.sleep(.6)
        run(['swaymsg','[app_id="bharta-fixture" title="second window"] move container to workspace 1'])
        time.sleep(.7)
        tree=json.loads(run(['swaymsg','-t','get_tree']).stdout)
        def fixture_ids(node):
            result=[]
            if node.get('app_id')=='bharta-fixture' and node.get('foreign_toplevel_identifier'):
                result.append((node.get('name',''),node['foreign_toplevel_identifier']))
            for child in node.get('nodes',[])+node.get('floating_nodes',[]): result.extend(fixture_ids(child))
            return result
        identifiers=dict(fixture_ids(tree))
        # A transparent part of the open layer must not swallow desktop clicks.
        run(['swaymsg','[app_id="bharta-fixture" title="second window"] focus'])
        preview=probe(120,14,'--hover',1500);time.sleep(.6)
        preview.terminate();preview.wait()
        desktop_click=probe(600,100);desktop_click.wait()
        def focused_name(node):
            if node.get('focused'):return node.get('name')
            return next((name for child in node.get('nodes',[])+node.get('floating_nodes',[])
                         if (name:=focused_name(child))),None)
        assert focused_name(json.loads(run(['swaymsg','-t','get_tree']).stdout))=='Preview fixture — wide window', 'Popup intercepts input outside its visible bounds'
        if len(identifiers)==2:
            Path(env['BHARTA_TEST_ANIMATE']).touch()
            # Observe the displayed thumbnail after the opening fade has finished.
            # Capture throughput alone cannot catch a stale rasterized texture.
            live=probe(120,14,'--hover',8000)
            time.sleep(.8)
            animated=[];static=[]
            for frame in range(6):
                shot(f'preview-live-{frame}')
                displayed=Image.open(DEST/f'preview-live-{frame}.png').convert('RGB')
                animated.append(displayed.crop((25,75,185,220)).tobytes())
                static.append(displayed.crop((245,75,385,220)).tobytes())
                time.sleep(.12)
            assert len(set(animated))>=4, 'Displayed preview freezes when the opening fade finishes'
            assert len(set(static))==1, 'Static source thumbnail changes without source damage'
            live.wait()
            leave=probe(600,600,'--hover',900);leave.wait()
            benchmark=[str(ROOT/'target/release/examples/capture_perf'),identifiers['Preview fixture — wide window'],identifiers['Preview fixture — second window']]
            performance=subprocess.run(benchmark,cwd=ROOT,env=env,capture_output=True,text=True)
            # Capture timing is sensitive to unrelated host load. Keep the
            # measurement and allow one fresh sample before reporting failure.
            if performance.returncode:
                (DEST/'preview-performance-first.log').write_text(performance.stdout+performance.stderr)
                performance=subprocess.run(benchmark,cwd=ROOT,env=env,capture_output=True,text=True)
            (DEST/'preview-performance.log').write_text(performance.stdout)
            assert performance.returncode==0, performance.stdout+performance.stderr
        fixture.terminate();fixture.wait();Path(env['BHARTA_TEST_WINDOW_PID']).unlink(missing_ok=True);time.sleep(1.2)
        outside=probe(600,600,'--hover',2600);outside.wait()
        player=subprocess.Popen([str(ROOT / 'target/release/examples/headless_player')],cwd=ROOT,env=env,stdin=subprocess.PIPE,stdout=open(DEST / 'player.log','w'),stderr=subprocess.STDOUT,start_new_session=True)
        processes.append(player);time.sleep(2)
        hover_music=probe(sound_x,14,'--hover',1600);time.sleep(.8);shot('music-hover');hover_music.wait()
        assert Image.open(DEST / 'music-hover.png').convert('RGB').getpixel((1250,100)) != (0,0,0), 'Music hover failed'
        leave=probe(600,600,'--hover',1000);leave.wait()
        music=probe(sound_x,14,'--click-hold',6000);time.sleep(1.4)
        shot('playing');audio_before=state.read_text()
        # Fake parec gives each sink input a distinct tone. Each mini-EQ must
        # show only the frequency band for its own source.
        playing_image=Image.open(DEST / 'playing.png').convert('RGB')
        def eq_heights(center):
            candidates=[]
            # Anchor to the source row, not the screen: clock/date and battery
            # text move the popup horizontally across machines and days.
            for left,right in ((control_right-84,control_right-30),):
                columns=[]
                for x in range(left,right):
                    ys=[y for y in range(center-10,center+11)
                        if min(playing_image.getpixel((x,y)))>120]
                    if ys:
                        columns.append((x,max(ys)-min(ys)))
                bars=[]
                for x,height in columns:
                    if not bars or x>bars[-1][-1][0]+1:
                        bars.append([])
                    bars[-1].append((x,height))
                candidates.append([max(height for _,height in bar)
                                   for bar in bars if len(bar)<=3])
            return next((candidate for candidate in candidates if len(candidate)==7),candidates[0])
        def image_bounds(color):
            pixels=[(x,y) for y in range(28,550) for x in range(1050,1300)
                    if playing_image.getpixel((x,y))==color]
            assert pixels, f'Source artwork missing: {color}'
            xs,ys=zip(*pixels)
            return min(xs),min(ys),max(xs),max(ys)
        firefox_art=image_bounds((220,105,40))
        spotify_art=image_bounds((40,180,90))
        assert firefox_art[0]==spotify_art[0] and firefox_art[2]==spotify_art[2], 'Source images do not align'
        assert firefox_art[2]-firefox_art[0]<=28 and firefox_art[3]-firefox_art[1]<=28, 'Artwork made source row oversized'
        firefox_center=(firefox_art[1]+firefox_art[3])//2
        spotify_center=(spotify_art[1]+spotify_art[3])//2
        control_right=firefox_art[0]+312
        def playback_controls(center):
            return playing_image.crop((control_right-84,center+28,control_right,center+50))
        assert ImageChops.difference(playback_controls(firefox_center),playback_controls(spotify_center)).getbbox() is None, 'Long source summary moved playback controls'
        firefox_eq=eq_heights(firefox_center)
        spotify_eq=eq_heights(spotify_center)
        assert len(firefox_eq)==7 and firefox_eq[3]>=10 and max(firefox_eq[:3]+firefox_eq[4:])<=5, f'Firefox EQ did not isolate 750 Hz: {firefox_eq}'
        assert len(spotify_eq)==7 and spotify_eq[4]>=10 and max(spotify_eq[:4]+spotify_eq[5:])<=5, f'Spotify EQ did not isolate 1600 Hz: {spotify_eq}'
        player.stdin.write(b'pause\n');player.stdin.flush();time.sleep(1.2)
        shot('paused')
        paused_image=Image.open(DEST / 'paused.png').convert('RGB')
        playing_image=paused_image
        paused_firefox_eq,paused_spotify_eq=eq_heights(firefox_center),eq_heights(spotify_center)
        assert paused_firefox_eq[3]>=10 and max(paused_firefox_eq[:3]+paused_firefox_eq[4:])<=5, 'Firefox meter stopped when unrelated MPRIS playback paused'
        assert paused_spotify_eq[4]>=10 and max(paused_spotify_eq[:4]+paused_spotify_eq[5:])<=5, 'Spotify meter stopped when unrelated MPRIS playback paused'
        playing=Image.open(DEST / 'playing.png').convert('RGB');paused=Image.open(DEST / 'paused.png').convert('RGB')
        # Exclude the popup's drop shadow at y=27, which varies as its fade
        # animation advances; compare only the actual bar surface.
        controls=(int(sound_x)-16,0,int(wifi_x)+16,26)
        assert ImageChops.difference(playing.crop(controls),paused.crop(controls)).getbbox() is None, 'Playback moved bar controls'
        assert playing.getpixel((1250,100)) != (0,0,0), 'Music popup missing'
        assert playing.crop((0,28,1600,600)).getbbox()==paused.crop((0,28,1600,600)).getbbox(), 'Playback moved popup'
        assert state.read_text()==audio_before, 'Pause changed volume'
        click=probe(600,600,'--click-hold',600);time.sleep(.23);shot('click-fade');click.wait();time.sleep(.3);shot('click-dismissed')
        assert Image.open(DEST / 'click-dismissed.png').crop((0,28,1600,600)).getbbox() is None, 'Outside click failed to dismiss'
        music.wait()
        wifi=probe(wifi_x,14,'--hover',2000);time.sleep(.8);shot('wifi-hover');wifi.wait()
        assert Image.open(DEST / 'wifi-hover.png').convert('RGB').getpixel((wifi_x-60,50)) != (0,0,0), 'Wi-Fi hover failed'
        session=probe(28,14,'--hover',1600);time.sleep(.8);shot('session-hover');session.wait()
        switched=Image.open(DEST / 'session-hover.png').convert('RGB')
        assert switched.getpixel((50,55)) != (0,0,0), 'Session hover failed'
        assert switched.crop((500,28,1600,900)).getbbox() is None, 'Switching menus left the old popup visible'
        # Switch directly from a clicked (grabbed) menu to a workspace preview,
        # then to another menu, without visiting the desktop between controls.
        leave=probe(600,600,'--hover',900);leave.wait()
        clicked=probe(72,14,'--click-hold',900);clicked.wait()
        switch_preview=probe(125,14,'--hover',1000);time.sleep(.65);shot('switch-preview');switch_preview.wait()
        assert Image.open(DEST / 'switch-preview.png').convert('RGB').crop((0,380,500,900)).getbbox() is None, 'Clicked Apps did not switch to workspace preview'
        assert Image.open(DEST / 'switch-preview.png').convert('RGB').getpixel((50,50)) != (0,0,0), 'Workspace hover did not open'
        switch_sound=probe(sound_x,14,'--hover',1000);time.sleep(.65);shot('switch-sound');switch_sound.wait()
        switched=Image.open(DEST / 'switch-sound.png').convert('RGB')
        assert switched.getpixel((1250,80)) != (0,0,0), 'Workspace preview did not switch to Sound'
        assert switched.crop((0,28,500,900)).getbbox() is None, 'Old workspace preview stayed visible'
        # Promote the hovered menu to clicked, then toggle it closed. Remaining
        # over the same button must not immediately reopen it.
        background=switched.getpixel((1250,80))
        promote=probe(sound_x,14,'--click-hold',2500)
        deadline=time.monotonic()+2.0
        frame=0
        while time.monotonic()<deadline:
            name=f'click-stable-{frame}'
            shot(name)
            current=Image.open(DEST / (name+'.png')).convert('RGB').getpixel((1250,80))
            # The switched menu can still be fading in when sampled above.
            # Promoting it may finish that fade, but must never reduce opacity.
            assert all(after>=before for after,before in zip(current,background)), 'Click blinked or restarted the popup fade'
            background=current
            frame+=1
            time.sleep(.02)
        assert frame>=4, 'Too few frames to check click stability'
        promote.wait()
        toggle=probe(sound_x,14,'--click-hold',1000);time.sleep(.85);shot('toggle-closed');toggle.wait()
        assert Image.open(DEST / 'toggle-closed.png').convert('RGB').crop((0,28,1600,900)).getbbox() is None, 'Hover reopened a menu toggled closed'
        # A second real bar on another private output must dismiss the first.
        run(['swaymsg','create_output'])
        run(['swaymsg','output HEADLESS-2 mode 1600x900 pos 1600 0'])
        second=start([str(BAR),'--dark','--output','HEADLESS-2'],'second-bar.log');time.sleep(2)
        first=probe(72,14,'--click-hold',2000);time.sleep(.8)
        # Pin an actual search so pointer leave alone cannot explain dismissal.
        typed=probe(72,14,'--keys-only',30);typed.wait();first.wait()
        shot('pinned-search')
        assert Image.open(DEST / 'pinned-search.png').convert('RGB').getpixel((50,100)) != (0,0,0), 'Pinned search missing'
        other=probe(sound_x,14,'--hover',2500,output='HEADLESS-2');time.sleep(1)
        shot('single-popup')
        run(['grim','-o','HEADLESS-2',str(DEST / 'second-popup.png')])
        assert Image.open(DEST / 'second-popup.png').convert('RGB').getpixel((1250,80)) != (0,0,0), 'Second output did not open its popup'
        assert Image.open(DEST / 'single-popup.png').convert('RGB').crop((0,28,1600,900)).getbbox() is None, 'Other output left a popup open'
        assert Image.open(DEST / 'single-popup.png').convert('RGB').getpixel((110,8)) == Image.open(DEST / 'bar.png').convert('RGB').getpixel((110,8)), 'Workspace highlight changed when focus moved to the other output'
        other.wait()
        # The same output resized to a large display gets a larger preview.
        run(['swaymsg','output HEADLESS-2 mode 3840x2160'])
        time.sleep(.5)
        large=probe(120,14,'--hover',1500,output='HEADLESS-2',size=(3840,2160));time.sleep(.8)
        run(['grim','-o','HEADLESS-2',str(DEST/'large-preview.png')])
        large_image=Image.open(DEST/'large-preview.png').convert('RGB')
        assert large_image.size==(3840,2160), 'Large output did not resize'
        row=[x for x in range(1200) if large_image.getpixel((x,50))!=(0,0,0)]
        assert row and 950<=max(row)-min(row)+1<=1000, 'Workspace preview does not scale with its display'
        large.wait()
        for log in ('bar.log','second-bar.log'):
            assert 'Keyboard map:' not in (DEST/log).read_text(), f'Keyboard map failed on {log}'
        assert second.poll() is None, 'Second bar exited'
        assert bar.poll() is None, 'egui bar exited'
        # A real XDG config changes geometry, palette, timing, and typography.
        # The desktop user's settings never enter this private session.
        second.terminate();second.wait()
        config_dir=Path(env['XDG_CONFIG_HOME'])/'bharta'
        config_dir.mkdir(parents=True,exist_ok=True)
        (config_dir/'config.json').write_text(json.dumps({
            'appearance':{'dark':True,'font_size':13},
            'layout':{'bar_height':34,'group_gap':12,'sound_max_height':200},
            'animation':{'preview_fade_ms':80},
            'colors':{'dark':{'bar':'#26313b','popup':'#29343e'}},
        }))
        configured=start([str(BAR),'--output','HEADLESS-2'],'configured-bar.log');time.sleep(1.5)
        run(['grim','-o','HEADLESS-2',str(DEST/'configured-bar.png')])
        configured_image=Image.open(DEST/'configured-bar.png').convert('RGB')
        assert configured_image.getpixel((2000,33))==(38,49,59), 'XDG configuration did not set bar color and height'
        assert configured_image.getpixel((2000,34))==(0,0,0), 'Configured bar height was not respected'
        custom_preview=probe(120,17,'--hover',1500,output='HEADLESS-2',size=(3840,2160));time.sleep(.7)
        run(['grim','-o','HEADLESS-2',str(DEST/'configured-preview.png')])
        assert Image.open(DEST/'configured-preview.png').convert('RGB').getpixel((10,80))==(41,52,62), 'Configured popup palette was not applied'
        custom_preview.wait()
        configured_battery=next(x for x in range(3200,3800)
            if all(min(configured_image.getpixel((dx,12)))>100 for dx in range(x,x+16)))
        custom_sound=probe(configured_battery-68,17,'--hover',1800,output='HEADLESS-2',size=(3840,2160));time.sleep(.8)
        run(['grim','-o','HEADLESS-2',str(DEST/'configured-sound.png')])
        configured_sound=Image.open(DEST/'configured-sound.png').convert('RGB')
        drawer_bounds=configured_sound.crop((3000,34,3840,2160)).getbbox()
        assert drawer_bounds and 220<=drawer_bounds[3]<=224, f'Configured Sound height not applied: {drawer_bounds}'
        custom_sound.wait()
        assert configured.poll() is None, 'Configured bar exited'
        print('Headless screenshots and logs:', DEST)
    finally:
        for p in reversed(processes):
            if p.poll() is None:
                os.killpg(p.pid, signal.SIGTERM)
                p.wait(timeout=5)
        if 'bus' in locals():
            os.kill(int(bus[1]), signal.SIGTERM)
