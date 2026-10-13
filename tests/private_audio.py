#!/usr/bin/env python3
"""Run native audio controls against private PipeWire-Pulse and null outputs.
Requires existing pipewire, pipewire-pulse, wireplumber, and cargo commands.
Never connects to desktop audio or enables hardware monitors.
"""
import os, subprocess, tempfile, time
from pathlib import Path
with tempfile.TemporaryDirectory(prefix='bharta-private-pulse-') as directory:
    runtime=Path(directory)
    (runtime/'core.conf').write_text('''context.properties = { core.daemon = true core.name = pipewire-0 support.dbus = false }
context.spa-libs = { audio.convert.* = audioconvert/libspa-audioconvert support.* = support/libspa-support }
context.modules = [ { name = libpipewire-module-protocol-native } { name = libpipewire-module-client-node } { name = libpipewire-module-adapter } { name = libpipewire-module-link-factory } { name = libpipewire-module-spa-node-factory } { name = libpipewire-module-metadata } { name = libpipewire-module-access args = { access.force = unrestricted } } ]
''')
    env=os.environ.copy()
    for key in ['PIPEWIRE_REMOTE','PULSE_SERVER','PULSE_RUNTIME_PATH','PULSE_COOKIE','DBUS_SESSION_BUS_ADDRESS','DBUS_SYSTEM_BUS_ADDRESS']:
        env.pop(key,None)
    env.update(XDG_RUNTIME_DIR=directory,PIPEWIRE_RUNTIME_DIR=directory,PULSE_RUNTIME_PATH=directory+'/pulse',PULSE_SERVER='unix:'+directory+'/pulse/native',DBUS_SESSION_BUS_ADDRESS='unix:path='+directory+'/no-bus',DBUS_SYSTEM_BUS_ADDRESS='unix:path='+directory+'/no-bus')
    wpconfig=runtime/'settings/wireplumber/wireplumber.conf.d'
    wpconfig.mkdir(parents=True)
    (wpconfig/'isolation.conf').write_text('wireplumber.profiles = { policy = { hardware.audio = disabled hardware.bluetooth = disabled hardware.video-capture = disabled monitor.alsa = disabled monitor.bluez = disabled monitor.bluez-midi = disabled monitor.alsa-midi = disabled monitor.v4l2 = disabled monitor.libcamera = disabled support.portal-permissionstore = disabled support.reserve-device = disabled } }')
    env['XDG_CONFIG_HOME']=str(runtime/'settings')
    env['XDG_STATE_HOME']=str(runtime/'state')
    processes=[]
    try:
        log=open(str(runtime/'server.log'),'w')
        processes.append(subprocess.Popen(['pipewire','-c',str(runtime/'core.conf')],env=env,stdout=log,stderr=subprocess.STDOUT))
        for _ in range(100):
            if (runtime/'pipewire-0').exists():break
            time.sleep(.05)
        processes.append(subprocess.Popen(['pipewire-pulse','-c','/usr/share/pipewire/pipewire-pulse.conf'],env=env,stdout=log,stderr=subprocess.STDOUT))
        for _ in range(100):
            if (runtime/'pulse/native').exists():break
            time.sleep(.05)
        assert (runtime/'pulse/native').exists(),'private pulse server did not start'
        processes.append(subprocess.Popen(['wireplumber','--profile=policy'],env=env,stdout=log,stderr=subprocess.STDOUT))
        time.sleep(1)
        assert processes[-1].poll() is None,'private policy daemon did not start'
        result=subprocess.run(['cargo','test','--offline','volume::tests::live_volume_and_mute_roundtrip','--','--ignored','--exact'],env=env,text=True,capture_output=True)
        print(result.stdout,result.stderr)
        if result.returncode:
            log.flush()
            print((runtime/'server.log').read_text())
        raise SystemExit(result.returncode)
    finally:
        for process in reversed(processes):
            process.terminate()
            try:process.wait(timeout=5)
            except subprocess.TimeoutExpired:process.kill();process.wait()
