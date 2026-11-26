# Reads lane transcripts under ~/.claude/projects: first-prompt -> first synapse channel event, for mail-started sessions. Run: python -I tools/mail-first-latency.py (cwd ~/.claude/projects). Read-only.
import os
import json,glob,sys,datetime as D
def ts(s): return D.datetime.fromisoformat(s.replace('Z','+00:00'))
def text(m):
    c=m.get('message',{}).get('content')
    if isinstance(c,str): return c
    if isinstance(c,list): return ' '.join(x.get('text','') for x in c if isinstance(x,dict))
    return ''
rows=[]
for f in glob.glob('C--Projects-*/*.jsonl'):
    first=None;chan=None;firstasst=None
    try:
        for line in open(f,encoding='utf-8',errors='replace'):
            try: e=json.loads(line)
            except: continue
            t=e.get('timestamp')
            if not t: continue
            if e.get('type')=='user':
                x=text(e)
                if first is None:
                    if 'You were started because mail arrived' in x: first=(ts(t),x[:60])
                    else: break
                elif chan is None and '<channel source="synapse"' in x:
                    chan=ts(t)
            elif e.get('type')=='assistant' and first and firstasst is None:
                firstasst=ts(t)
    except Exception as ex: continue
    if first:
        rows.append((first[0],os.path.basename(os.path.dirname(f)),(chan-first[0]).total_seconds() if chan else None,(firstasst-first[0]).total_seconds() if firstasst else None))
rows.sort()
print(len(rows),'mail-started sessions with first prompt = mail prompt')
for r in rows[-40:]: print(r[0].strftime('%m-%d %H:%M:%S'),r[1][11:],'first_channel_s=',r[2],'first_assistant_s=',r[3])
import statistics
v=[r[2] for r in rows if r[2] is not None]
print('with channel event:',len(v),'median',statistics.median(v) if v else None,'max',max(v) if v else None)
import collections
b=collections.Counter()
for x in v:
    k='<30' if x<30 else '30-110' if x<110 else '110-130 (2min redelivery)' if x<130 else '130-230' if x<230 else '230-250 (4min)' if x<250 else '>250'
    b[k]+=1
print(dict(b),'of',len(v),'; no channel event:',len(rows)-len(v),'; min',min(v))
print('first assistant turn after prompt: median',statistics.median([r[3] for r in rows if r[3]]))
