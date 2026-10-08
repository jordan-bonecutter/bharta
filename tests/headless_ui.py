#!/usr/bin/env python3
"""Isolated GTK/Sway UI harness. Never connects input to the desktop session.
Build first: cargo build --release --examples && cargo build --release
Run: python3 tests/headless_ui.py /tmp/bharta-headless
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
DEST = Path(sys.argv[1] if len(sys.argv) > 1 else '/tmp/bharta-headless').resolve()
DEST.mkdir(parents=True, exist_ok=True)
processes = []
with tempfile.TemporaryDirectory(prefix='bharta-headless-') as directory:
    runtime = Path(directory)
    runtime.chmod(0o700)
    env = os.environ.copy()
    for key in ['SWAYSOCK', 'WAYLAND_DISPLAY', 'DISPLAY', 'DBUS_SESSION_BUS_ADDRESS']:
        env.pop(key, None)
    env.update(XDG_RUNTIME_DIR=str(runtime), WLR_BACKENDS='headless', GTK_A11Y='none', GTK_IM_MODULE='simple', GDK_BACKEND='wayland', GSK_RENDERER='cairo', BHARTA_HEADLESS='1',
               WLR_RENDERER='pixman', WLR_HEADLESS_OUTPUTS='1',
               DBUS_SYSTEM_BUS_ADDRESS='unix:path=' + str(runtime / 'no-system-bus'),
               PULSE_SERVER='unix:' + str(runtime / 'no-audio-server'))
    state = runtime / 'volume.json'
    state.write_text('[32768, 32768]')
    fake = runtime / 'pactl'
    fake.write_text('''#!/usr/bin/env python3
import json,sys,os
from pathlib import Path
p=Path(os.environ['BHARTA_TEST_AUDIO'])
a=sys.argv[1:]
if a==['get-default-sink']:print('test')
elif a==['--format=json','list','sinks']:
 v=json.loads(p.read_text());print(json.dumps([{'name':'test','description':'Test speakers','channel_map':'front-left,front-right','volume':{k:{'value':n} for k,n in zip(['front-left','front-right'],v)},'ports':[{'name':'speaker','description':'Speakers','availability':'yes'}],'active_port':'speaker','mute':False}]))
elif a and a[0]=='set-sink-volume':p.write_text(json.dumps([int(n) for n in a[2:]]))
elif 'list' in a:print('[]')
''')
    fake.chmod(0o755)
    env['PATH'] = str(runtime) + os.pathsep + env['PATH']
    env['BHARTA_TEST_AUDIO'] = str(state)
    config = runtime / 'config'
    config.write_text('output HEADLESS-1 mode 1600x900\nseat seat0 fallback true\n')
    def start(args, log):
        p = subprocess.Popen(args, cwd=ROOT, env=env, stdout=open(DEST / log, 'w'),
                             stderr=subprocess.STDOUT, start_new_session=True)
        processes.append(p)
        return p
    def run(args):
        return subprocess.run(args, cwd=ROOT, env=env, check=True, capture_output=True, text=True)
    def probe(x, y, *args, output='HEADLESS-1'):
        return start([str(ROOT / 'target/release/examples/ui_probe'), output,
                      str(x), str(y), '1600', '900', *map(str, args)], 'input.log')
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
        bar = start([str(ROOT / 'target/release/bharta'), '--dark', '--output', 'HEADLESS-1'], 'bar.log')
        for _ in range(100):
            assert bar.poll() is None, 'GTK bar exited before mapping'
            shot('bar')
            if Image.open(DEST / 'bar.png').getpixel((800,10))[:3] != (0,0,0):
                break
            time.sleep(.2)
        else:
            raise AssertionError('GTK bar did not map; see bar.log')
        time.sleep(2)
        # All input is scoped to HEADLESS-1 on the private Wayland socket.
        holder = probe(1345, 14, '--click-hold', 6000)
        time.sleep(1)
        shot('sound')
        assert Image.open(DEST / 'sound.png').getpixel((1250,80))[:3] != (0,0,0), 'Sound popup missing'
        pixels=Image.open(DEST / 'sound.png').convert('RGB')
        # Find the native scale's long, bright filled trough, independent of
        # typography and spacing tweaks.
        runs=[]
        for y in range(65,160):
            begin=None
            for x in range(1100,1550):
                bright=min(pixels.getpixel((x,y)))>150
                if bright and begin is None: begin=x
                if not bright and begin is not None:
                    runs.append((x-begin,begin,y));begin=None
        length,left,slider_y=max(runs)
        assert length>60, 'Native volume scale not found'
        drag=probe(left+length-5,slider_y,'--drag',left+45,slider_y)
        time.sleep(1.1)
        during=json.loads(state.read_text())
        assert during[0] < 32768, f'Audio did not change while dragging: {during}'
        drag.wait();time.sleep(.8)
        final=json.loads(state.read_text())
        assert 0 < final[0] <= during[0] < 32768, 'Final slider position was lost'
        time.sleep(.3)
        assert json.loads(state.read_text())==final, 'Volume changed after commands settled'
        hover=probe(1580,14,'--hover',4000)
        time.sleep(3)
        shot('bar-hover')
        assert Image.open(DEST / 'bar-hover.png').getpixel((1250,80))[:3] != (0,0,0), 'Popup closed over the bar'
        hover.wait();holder.wait()
        outside=probe(600,600,'--hover',4000)
        time.sleep(3)
        shot('dismissed')
        assert Image.open(DEST / 'dismissed.png').getpixel((1250,80))[:3] == (0,0,0), 'Popup did not dismiss'
        outside.wait()
        apps=probe(72,14,'--hover',2500);time.sleep(1);shot('launcher');apps.wait()
        escape=probe(160,90,1);escape.wait();time.sleep(.5);shot('escape')
        assert Image.open(DEST / 'escape.png').convert('RGB').crop((0,28,1600,900)).getbbox() is None, 'Escape did not close launcher'
        fixture=start([str(ROOT / 'target/release/examples/headless_window')],'windows.log');time.sleep(2);shot('fixture')
        preview=probe(120,14,'--hover',2500);time.sleep(1);shot('workspace');preview.wait()
        # The preview stays bounded even with full-sized captured windows.
        # Its rightmost edge must remain within the small left-side popup.
        image=Image.open(DEST / 'workspace.png').convert('RGB')
        baseline=Image.open(DEST / 'fixture.png').convert('RGB')
        bounds=ImageChops.difference(image,baseline).crop((0,28,1600,900)).getbbox()
        assert bounds and bounds[2]-bounds[0]<=380 and bounds[3]-bounds[1]<=300, f'Preview is oversized: {bounds}'
        fixture.terminate();fixture.wait();time.sleep(.3)
        outside=probe(600,600,'--hover',2600);outside.wait()
        player=subprocess.Popen([str(ROOT / 'target/release/examples/headless_player')],cwd=ROOT,env=env,stdin=subprocess.PIPE,stdout=open(DEST / 'player.log','w'),stderr=subprocess.STDOUT,start_new_session=True)
        processes.append(player);time.sleep(2)
        hover_music=probe(1260,14,'--hover',1600);time.sleep(.8);shot('music-hover');hover_music.wait()
        assert Image.open(DEST / 'music-hover.png').convert('RGB').getpixel((1250,100)) != (0,0,0), 'Music hover failed'
        leave=probe(600,600,'--hover',1000);leave.wait()
        music=probe(1260,14,'--click-hold',6000);time.sleep(1)
        shot('playing');audio_before=state.read_text()
        player.stdin.write(b'pause\n');player.stdin.flush();time.sleep(1.2)
        shot('paused')
        playing=Image.open(DEST / 'playing.png').convert('RGB');paused=Image.open(DEST / 'paused.png').convert('RGB')
        assert ImageChops.difference(playing.crop((1245,0,1390,28)),paused.crop((1245,0,1390,28))).getbbox() is None, 'Playback moved bar controls'
        assert playing.getpixel((1250,100)) != (0,0,0), 'Music popup missing'
        assert playing.crop((0,28,1600,600)).getbbox()==paused.crop((0,28,1600,600)).getbbox(), 'Playback moved popup'
        assert state.read_text()==audio_before, 'Pause changed volume'
        click=probe(600,600,'--click-hold',600);time.sleep(.23);shot('click-fade');click.wait();time.sleep(.3);shot('click-dismissed')
        assert Image.open(DEST / 'click-dismissed.png').crop((0,28,1600,600)).getbbox() is None, 'Outside click failed to dismiss'
        music.wait()
        wifi=probe(1298,14,'--hover',2000);time.sleep(.8);shot('wifi-hover');wifi.wait()
        assert Image.open(DEST / 'wifi-hover.png').convert('RGB').getpixel((1250,50)) != (0,0,0), 'Wi-Fi hover failed'
        session=probe(28,14,'--hover',1600);time.sleep(.8);shot('session-hover');session.wait()
        switched=Image.open(DEST / 'session-hover.png').convert('RGB')
        assert switched.getpixel((50,55)) != (0,0,0), 'Session hover failed'
        assert switched.crop((500,28,1600,900)).getbbox() is None, 'Switching menus left the old popup visible'
        # A second real bar on another private output must dismiss the first.
        run(['swaymsg','create_output'])
        run(['swaymsg','output HEADLESS-2 mode 1600x900 pos 1600 0'])
        second=start([str(ROOT / 'target/release/bharta'),'--dark','--output','HEADLESS-2'],'second-bar.log');time.sleep(2)
        first=probe(72,14,'--click-hold',2000);time.sleep(.8)
        # Pin an actual search so pointer leave alone cannot explain dismissal.
        typed=probe(100,82,30);typed.wait();first.wait()
        shot('pinned-search')
        assert Image.open(DEST / 'pinned-search.png').convert('RGB').getpixel((50,100)) != (0,0,0), 'Pinned search missing'
        other=probe(1345,14,'--hover',2500,output='HEADLESS-2');time.sleep(1)
        shot('single-popup')
        run(['grim','-o','HEADLESS-2',str(DEST / 'second-popup.png')])
        assert Image.open(DEST / 'second-popup.png').convert('RGB').getpixel((1250,80)) != (0,0,0), 'Second output did not open its popup'
        assert Image.open(DEST / 'single-popup.png').convert('RGB').crop((0,28,1600,900)).getbbox() is None, 'Other output left a popup open'
        other.wait()
        assert second.poll() is None, 'Second bar exited'
        assert bar.poll() is None, 'GTK bar exited' 
        print('Headless screenshots and logs:', DEST)
    finally:
        for p in reversed(processes):
            if p.poll() is None:
                os.killpg(p.pid, signal.SIGTERM)
                p.wait(timeout=5)
        if 'bus' in locals():
            os.kill(int(bus[1]), signal.SIGTERM)
