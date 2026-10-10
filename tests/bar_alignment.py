"""Pixel-level bar alignment contracts, independent of the Rust layout helper."""
from PIL import Image


def assert_bar_alignment(image, settings, scale=1):
    image = image.convert('RGB')
    if scale != 1:
        image = image.resize((image.width // scale, image.height // scale), Image.Resampling.NEAREST)
    layout = settings['layout']
    height = layout['bar_height']
    slot = layout['icon_slot_width']
    gap = layout['group_gap']
    icon_size = settings['appearance']['icon_size']
    background = image.getpixel((image.width // 2, 0))

    def icon_pixel(x, y):
        rgb = image.getpixel((x, y))
        # Icons are monochrome. Ignore colored plots and their faint baseline.
        return max(rgb) - min(rgb) < 8 and max(abs(a-b) for a, b in zip(rgb, background)) > 64

    battery_y = int(height / 2 - layout['battery_icon_height'] / 2)
    battery = next((x for x in range(image.width // 2, image.width - 16)
                    if all(icon_pixel(dx, battery_y) for dx in range(x, x+16))), None)
    assert battery is not None, 'Cannot locate battery for bar alignment checks'
    wifi = battery - gap - slot
    bluetooth = wifi - gap - slot
    sound = bluetooth - gap - layout['sound_tray_width']
    cpu = sound - gap - layout['cpu_width']
    controls = {
        'CPU': (cpu, layout['cpu_width'], True),
        'Sound': (sound, layout['sound_tray_width'], True),
        'Bluetooth': (bluetooth, slot, False),
        'Apps': (layout['bar_margin'] + layout['session_width'] + gap, layout['apps_width'], False),
        'Session': (layout['bar_margin'], layout['session_width'], False),
    }
    for name, (left, width, has_plot) in controls.items():
        pixels = [(x, y) for y in range(height) for x in range(int(left), int(left + width))
                  if icon_pixel(x, y)]
        assert pixels, f'{name} icon missing'
        xs, ys = zip(*pixels)
        center_x = left + (min(slot, width) if has_plot else width) / 2
        actual_x = (min(xs) + max(xs) + 1) / 2
        actual_y = (min(ys) + max(ys) + 1) / 2
        assert abs(actual_x-center_x) <= 1.5, f'{name} icon center {actual_x}, expected {center_x}'
        assert abs(actual_y-height/2) <= 1.5, f'{name} icon is vertically misaligned: {actual_y}'
        size = min(icon_size, slot if has_plot else width, height)
        assert min(xs) >= center_x-size/2-1 and max(xs)+1 <= center_x+size/2+1, f'{name} icon escaped its slot'
        if has_plot and width > slot + 4:
            assert max(xs)+1 < left+slot, f'{name} icon overlaps its plot'
    return image, controls
