#!/usr/bin/env python3
"""Original Haiku landscape print and paintbrush (MIT OR Apache-2.0)."""
from icon_art import Drawing, INK, plane

d = Drawing()
p = plane(6, 7, 0.3)
d.shadow([(6,44),(45,56),(62,56),(30,44)])
d.shape([(45,19),(49,15),(49,52),(45,56)], d.gradient('acb5bd','657184'), INK)
d.shape(p([(0,0),(39,0),(39,37),(0,37)]), d.gradient('fffff7','d6dbdf'), INK)
d.shape(p([(4,4),(35,4),(35,28),(4,28)]), d.gradient('bedfe9','659dc3', (10,12),(42,45)))
d.ellipse(31, 26, 4, 4, d.gradient('fff4b4','e4b95c'))
d.shape(p([(4,24),(13,12),(23,26),(29,19),(35,25),(35,29),(4,29)]), d.gradient('90b39a','3b6b60'))
d.shape(p([(4,28),(18,21),(28,29),(35,26),(35,31),(4,31)]), d.gradient('78a49c','3d6b70'))
# Brush handle and ferrule; broad bristles identify editing rather than viewing.
d.shape([(36,43),(51,15),(55,13),(57,17),(43,47)], d.gradient('e6b998','a76442'), INK, 1.8)
d.shape([(35,43),(41,36),(47,40),(43,48)], d.gradient('f4f5e9','9eaeb9'), INK, 1.5)
d.shape([((35,43),(35,43),(30,45)), ((29,55),(30,51),(28,57)),
         ((42,49),(37,57),(44,46)), (43,48)], d.gradient('a8b6dc','475b8e'), INK, 1.8)
d.line([(33,51),(36,47)], 'dce6fa', 1.5)
d.save(__file__, 'photocraft', 'PhotoCraft')
