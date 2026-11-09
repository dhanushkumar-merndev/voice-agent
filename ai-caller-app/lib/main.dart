import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:url_launcher/url_launcher.dart';

import 'agent.dart';
import 'vapi.dart';

const Color brand = Color(0xFFC2410C);

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await AppSettings.load();
  runApp(const AiCallerApp());
  // Resume tracking calls that were still running when the app was closed
  for (final c in await Store.all()) {
    final created = DateTime.tryParse(c.createdAt);
    if (!c.hasReport && created != null && DateTime.now().difference(created).inMinutes < 30) {
      trackCall(c);
    }
  }
}

class AiCallerApp extends StatelessWidget {
  const AiCallerApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Star AI Caller',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        useMaterial3: true,
        colorScheme: ColorScheme.fromSeed(seedColor: brand, primary: brand),
        scaffoldBackgroundColor: const Color(0xFFFAFAF9),
        inputDecorationTheme: const InputDecorationTheme(
          border: OutlineInputBorder(borderRadius: BorderRadius.all(Radius.circular(10))),
          filled: true,
          fillColor: Colors.white,
          isDense: true,
        ),
      ),
      home: const HomeShell(),
    );
  }
}

class HomeShell extends StatefulWidget {
  const HomeShell({super.key});
  @override
  State<HomeShell> createState() => _HomeShellState();
}

class _HomeShellState extends State<HomeShell> {
  int tab = 0;

  void goTo(int i) => setState(() => tab = i);

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Star AI Caller', style: TextStyle(fontWeight: FontWeight.w700)),
        backgroundColor: Colors.white,
        surfaceTintColor: Colors.white,
      ),
      body: IndexedStack(
        index: tab,
        children: [
          CallPage(openSettings: () => goTo(2)),
          const HistoryPage(),
          SettingsPage(onSaved: () => setState(() {})),
        ],
      ),
      bottomNavigationBar: NavigationBar(
        selectedIndex: tab,
        onDestinationSelected: goTo,
        destinations: const [
          NavigationDestination(icon: Icon(Icons.phone_outlined), selectedIcon: Icon(Icons.phone), label: 'Call'),
          NavigationDestination(icon: Icon(Icons.headphones_outlined), selectedIcon: Icon(Icons.headphones), label: 'Recordings'),
          NavigationDestination(icon: Icon(Icons.settings_outlined), selectedIcon: Icon(Icons.settings), label: 'Settings'),
        ],
      ),
    );
  }
}

// ---------------------------------------------------------------- CALL PAGE

class CallPage extends StatefulWidget {
  final VoidCallback openSettings;
  const CallPage({super.key, required this.openSettings});
  @override
  State<CallPage> createState() => _CallPageState();
}

class _CallPageState extends State<CallPage> {
  late Sector sector;
  final company = TextEditingController();
  final offer = TextEditingController();
  final questions = TextEditingController();
  final customerName = TextEditingController();
  final number = TextEditingController();
  final city = TextEditingController();
  String language = AppSettings.language;
  bool busy = false;
  String? error;
  CallRecord? active;

  @override
  void initState() {
    super.initState();
    sector = sectors.firstWhere((s) => s.id == AppSettings.lastSector, orElse: () => sectors.first);
    _fillSector();
  }

  void _fillSector() {
    company.text = sector.defaultCompany;
    offer.text = sector.offer;
    questions.text = sector.questions.join('\n');
  }

  Future<void> _call() async {
    FocusScope.of(context).unfocus();
    setState(() => error = null);
    if (!AppSettings.configured) {
      setState(() => error = 'Add your Vapi key and Phone Number ID in Settings first.');
      return;
    }
    final phone = normalisePhone(number.text);
    if (phone == null) {
      setState(() => error = 'Enter a valid phone number, e.g. 98765 43210 or +919876543210');
      return;
    }
    final qs = questions.text.split('\n').map((q) => q.trim()).where((q) => q.isNotEmpty).toList();
    String? problem;
    if (company.text.trim().isEmpty) problem = 'Enter the company name the agent should say.';
    if (problem == null && offer.text.trim().isEmpty) problem = 'Describe what the company offers (Edit script).';
    if (problem == null && qs.isEmpty) problem = 'Add at least one question for the agent (Edit script).';
    if (problem != null) {
      setState(() => error = problem);
      return;
    }

    final req = CallRequest(
      sector: sector,
      company: company.text.trim(),
      offer: offer.text.trim(),
      questions: qs,
      customerName: customerName.text.trim(),
      number: phone,
      city: city.text.trim(),
      language: language,
      agentName: AppSettings.agentName.isEmpty ? 'Priya' : AppSettings.agentName,
      maleVoice: AppSettings.maleVoice,
      model: AppSettings.model.isEmpty ? 'gpt-4o-mini' : AppSettings.model,
      maxSeconds: AppSettings.maxSeconds.clamp(30, 600),
    );
    AppSettings.language = language;
    AppSettings.lastSector = sector.id;
    AppSettings.save();

    setState(() => busy = true);
    try {
      final res = await Vapi(AppSettings.apiKey).createCall(buildCallBody(req, AppSettings.phoneNumberId));
      final rec = CallRecord(
        id: '${res['id']}',
        number: phone,
        name: req.customerName,
        company: req.company,
        createdAt: DateTime.now().toIso8601String(),
        status: '${res['status'] ?? 'queued'}',
      );
      await Store.upsert(rec);
      setState(() => active = rec);
      trackCall(rec);
    } catch (e) {
      setState(() => error = e.toString());
    } finally {
      if (mounted) setState(() => busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        if (!AppSettings.configured)
          Card(
            color: const Color(0xFFFFF7ED),
            child: ListTile(
              leading: const Icon(Icons.info_outline, color: brand),
              title: const Text('First time? Add your Vapi key and phone number.'),
              trailing: TextButton(onPressed: widget.openSettings, child: const Text('Settings')),
            ),
          ),
        _Section(
          title: 'New AI call',
          children: [
            DropdownButtonFormField<String>(
              value: sector.id,
              decoration: const InputDecoration(labelText: 'Sector / script'),
              items: sectors.map((s) => DropdownMenuItem(value: s.id, child: Text(s.label))).toList(),
              onChanged: (v) => setState(() {
                sector = sectors.firstWhere((s) => s.id == v);
                _fillSector();
              }),
            ),
            const SizedBox(height: 12),
            TextField(controller: company, decoration: const InputDecoration(labelText: 'Company name the agent says')),
            const SizedBox(height: 12),
            TextField(controller: customerName, decoration: const InputDecoration(labelText: 'Customer name')),
            const SizedBox(height: 12),
            TextField(
              controller: number,
              keyboardType: TextInputType.phone,
              decoration: const InputDecoration(labelText: 'Phone number *', hintText: '98765 43210'),
            ),
            const SizedBox(height: 12),
            TextField(controller: city, decoration: const InputDecoration(labelText: 'City (optional)')),
            const SizedBox(height: 12),
            SegmentedButton<String>(
              segments: const [
                ButtonSegment(value: 'hinglish', label: Text('Hinglish')),
                ButtonSegment(value: 'english', label: Text('English')),
              ],
              selected: {language},
              onSelectionChanged: (s) => setState(() => language = s.first),
            ),
            const SizedBox(height: 4),
            ExpansionTile(
              key: ValueKey('edit-${sector.id}'),
              initiallyExpanded: sector.id == 'custom' || sector.defaultCompany.isEmpty,
              tilePadding: EdgeInsets.zero,
              title: const Text('Edit what the agent says and asks', style: TextStyle(color: brand, fontSize: 14)),
              children: [
                TextField(
                  controller: offer,
                  minLines: 3,
                  maxLines: 8,
                  decoration: const InputDecoration(labelText: 'About the company (the agent only uses these facts)'),
                ),
                const SizedBox(height: 12),
                TextField(
                  controller: questions,
                  minLines: 4,
                  maxLines: 12,
                  decoration: const InputDecoration(labelText: 'Questions to ask, one per line, in order'),
                ),
                const SizedBox(height: 8),
              ],
            ),
            const SizedBox(height: 8),
            SizedBox(
              width: double.infinity,
              child: FilledButton.icon(
                style: FilledButton.styleFrom(padding: const EdgeInsets.symmetric(vertical: 16)),
                onPressed: busy ? null : _call,
                icon: busy
                    ? const SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2, color: Colors.white))
                    : const Icon(Icons.call),
                label: Text(busy ? 'Placing call...' : 'Call now', style: const TextStyle(fontSize: 16)),
              ),
            ),
            if (error != null)
              Padding(
                padding: const EdgeInsets.only(top: 10),
                child: Text(error!, style: const TextStyle(color: Color(0xFFB91C1C))),
              ),
          ],
        ),
        if (active != null)
          ValueListenableBuilder<int>(
            valueListenable: storeVersion,
            builder: (_, __, ___) => _Section(title: 'Current call', children: [CallReportView(record: active!, live: true)]),
          ),
      ],
    );
  }
}

// ---------------------------------------------------------------- HISTORY

class HistoryPage extends StatelessWidget {
  const HistoryPage({super.key});

  @override
  Widget build(BuildContext context) {
    return ValueListenableBuilder<int>(
      valueListenable: storeVersion,
      builder: (context, _, __) => FutureBuilder<List<CallRecord>>(
        future: Store.all(),
        builder: (context, snap) {
          final calls = snap.data ?? [];
          if (snap.connectionState != ConnectionState.done && calls.isEmpty) {
            return const Center(child: CircularProgressIndicator());
          }
          if (calls.isEmpty) {
            return const Center(
              child: Padding(
                padding: EdgeInsets.all(32),
                child: Text('No calls yet. Make your first call from the Call tab.', textAlign: TextAlign.center),
              ),
            );
          }
          return ListView.builder(
            padding: const EdgeInsets.all(12),
            itemCount: calls.length + 1,
            itemBuilder: (context, i) {
              if (i == calls.length) {
                return TextButton(
                  onPressed: () async {
                    final ok = await showDialog<bool>(
                      context: context,
                      builder: (c) => AlertDialog(
                        title: const Text('Clear call history?'),
                        content: const Text('This only clears this phone. Recordings stay in your Vapi account.'),
                        actions: [
                          TextButton(onPressed: () => Navigator.pop(c, false), child: const Text('Cancel')),
                          TextButton(onPressed: () => Navigator.pop(c, true), child: const Text('Clear')),
                        ],
                      ),
                    );
                    if (ok == true) await Store.clear();
                  },
                  child: const Text('Clear history'),
                );
              }
              final c = calls[i];
              final created = DateTime.tryParse(c.createdAt)?.toLocal();
              return Card(
                color: Colors.white,
                child: ListTile(
                  title: Text(c.name.isNotEmpty ? c.name : c.number, style: const TextStyle(fontWeight: FontWeight.w600)),
                  subtitle: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text([if (c.name.isNotEmpty) c.number, c.company, if (created != null) _fmtDate(created)].join(' · ')),
                      const SizedBox(height: 4),
                      Wrap(spacing: 4, runSpacing: 4, children: _pills(c)),
                      if (c.summary != null)
                        Padding(
                          padding: const EdgeInsets.only(top: 6),
                          child: Text(c.summary!, maxLines: 3, overflow: TextOverflow.ellipsis, style: const TextStyle(color: Colors.black87)),
                        ),
                    ],
                  ),
                  isThreeLine: true,
                  onTap: () => Navigator.push(context, MaterialPageRoute(builder: (_) => CallDetailPage(record: c))),
                ),
              );
            },
          );
        },
      ),
    );
  }
}

class CallDetailPage extends StatelessWidget {
  final CallRecord record;
  const CallDetailPage({super.key, required this.record});

  @override
  Widget build(BuildContext context) {
    if (!record.hasReport && !record.ended) trackCall(record);
    return Scaffold(
      appBar: AppBar(title: Text(record.name.isNotEmpty ? record.name : record.number)),
      body: ValueListenableBuilder<int>(
        valueListenable: storeVersion,
        builder: (_, __, ___) => ListView(
          padding: const EdgeInsets.all(16),
          children: [CallReportView(record: record, live: !record.ended)],
        ),
      ),
    );
  }
}

// ---------------------------------------------------------------- REPORT VIEW

const Map<String, String> _statusLabels = {
  'queued': 'Dialling',
  'ringing': 'Ringing',
  'in-progress': 'On call',
  'forwarding': 'Transferring',
  'ended': 'Call ended',
};

List<Widget> _pills(CallRecord c) {
  final l = c.lead ?? {};
  final out = <Widget>[];
  final temp = l['lead_temperature']?.toString();
  if (temp != null) {
    final colors = {
      'hot': const [Color(0xFFFEE2E2), Color(0xFFB91C1C)],
      'warm': const [Color(0xFFFEF3C7), Color(0xFF92400E)],
      'cold': const [Color(0xFFE0F2FE), Color(0xFF075985)],
    }[temp];
    out.add(_Pill(temp.toUpperCase(), bg: colors?[0], fg: colors?[1]));
  }
  final outcome = l['call_outcome']?.toString();
  if (outcome != null) {
    out.add(_Pill(outcome.replaceAll('_', ' ')));
  } else if (!c.ended) {
    out.add(_Pill(_statusLabels[c.status] ?? c.status, bg: const Color(0xFFDCFCE7), fg: const Color(0xFF15803D)));
  } else if (c.endedReason != null) {
    out.add(_Pill(c.endedReason!.replaceAll(RegExp(r'[-_]'), ' ')));
  }
  return out;
}

String _fmtDate(DateTime d) {
  String two(int n) => n.toString().padLeft(2, '0');
  return '${two(d.day)}/${two(d.month)} ${two(d.hour)}:${two(d.minute)}';
}

class CallReportView extends StatelessWidget {
  final CallRecord record;
  final bool live;
  const CallReportView({super.key, required this.record, this.live = false});

  @override
  Widget build(BuildContext context) {
    final c = record;
    final l = c.lead ?? {};
    final order = ['queued', 'ringing', 'in-progress', 'ended'];
    final idx = order.indexOf(c.status).clamp(0, 3);
    final details = leadLabels.entries
        .where((e) => e.key != 'call_outcome' && e.key != 'lead_temperature' && l[e.key] != null && '${l[e.key]}'.isNotEmpty)
        .toList();
    final meta = [
      if (c.durationSeconds != null) '${c.durationSeconds}s',
      if (c.cost != null) '\$${c.cost!.toStringAsFixed(3)}',
    ].join(' · ');

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Expanded(
              child: Text(c.name.isNotEmpty ? '${c.name} · ${c.number}' : c.number,
                  style: const TextStyle(fontWeight: FontWeight.w600)),
            ),
            if (!c.ended) const SizedBox(width: 14, height: 14, child: CircularProgressIndicator(strokeWidth: 2)),
            const SizedBox(width: 6),
            Text(_statusLabels[c.status] ?? c.status, style: const TextStyle(fontSize: 13)),
          ],
        ),
        const SizedBox(height: 8),
        Row(
          children: List.generate(
            4,
            (i) => Expanded(
              child: Container(
                height: 6,
                margin: const EdgeInsets.symmetric(horizontal: 2),
                decoration: BoxDecoration(
                  color: i <= idx ? brand : const Color(0xFFE7E5E4),
                  borderRadius: BorderRadius.circular(4),
                ),
              ),
            ),
          ),
        ),
        const SizedBox(height: 8),
        if (!c.ended) const Text('Pick up the phone. The agent speaks first.', style: TextStyle(color: Colors.black54)),
        if (c.ended && !c.hasReport)
          const Text('Preparing the call report and recording (10-30 seconds)...', style: TextStyle(color: Colors.black54)),
        if (c.ended) ...[
          Wrap(spacing: 4, runSpacing: 4, crossAxisAlignment: WrapCrossAlignment.center, children: [
            ..._pills(c),
            if (meta.isNotEmpty) Text(meta, style: const TextStyle(color: Colors.black54, fontSize: 12)),
          ]),
          const _H('Call description'),
          Text(c.summary ?? 'Summary not available for this call.'),
          if (details.isNotEmpty) ...[
            const _H('Details captured'),
            Wrap(
              spacing: 8,
              runSpacing: 8,
              children: details.map((e) {
                final v = l[e.key];
                final text = v == true ? 'Yes' : v == false ? 'No' : '$v';
                return Container(
                  width: 150,
                  padding: const EdgeInsets.all(10),
                  decoration: BoxDecoration(color: const Color(0xFFF5F5F4), borderRadius: BorderRadius.circular(10)),
                  child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
                    Text(e.value, style: const TextStyle(fontSize: 11, color: Colors.black54)),
                    Text(text),
                  ]),
                );
              }).toList(),
            ),
          ],
          const _H('Recording'),
          Wrap(spacing: 8, runSpacing: 8, children: [
            if (c.recordingUrl != null)
              FilledButton.tonalIcon(
                onPressed: () => launchUrl(Uri.parse(c.recordingUrl!), mode: LaunchMode.externalApplication),
                icon: const Icon(Icons.play_arrow),
                label: const Text('Play / download'),
              )
            else
              const Text('No recording (the call may not have connected).', style: TextStyle(color: Colors.black54)),
            OutlinedButton.icon(
              onPressed: () async {
                await Clipboard.setData(ClipboardData(text: c.reportText()));
                if (context.mounted) {
                  ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('Report copied. Paste it in WhatsApp.')));
                }
              },
              icon: const Icon(Icons.copy),
              label: const Text('Copy report'),
            ),
            OutlinedButton.icon(
              onPressed: () => launchUrl(
                Uri.parse('https://wa.me/?text=${Uri.encodeComponent(c.reportText())}'),
                mode: LaunchMode.externalApplication,
              ),
              icon: const Icon(Icons.share),
              label: const Text('WhatsApp'),
            ),
          ]),
          const _H('Full conversation'),
          if (c.messages.isNotEmpty)
            ...c.messages.map((m) => _Bubble(m))
          else if (c.transcript != null)
            Text(c.transcript!)
          else
            const Text('No conversation was recorded.', style: TextStyle(color: Colors.black54)),
        ],
      ],
    );
  }
}

class _Bubble extends StatelessWidget {
  final ChatLine line;
  const _Bubble(this.line);
  @override
  Widget build(BuildContext context) {
    final agent = line.role == 'agent';
    return Align(
      alignment: agent ? Alignment.centerLeft : Alignment.centerRight,
      child: Container(
        constraints: BoxConstraints(maxWidth: MediaQuery.of(context).size.width * 0.78),
        margin: const EdgeInsets.symmetric(vertical: 4),
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
        decoration: BoxDecoration(
          color: agent ? const Color(0xFFFFF7ED) : const Color(0xFFF5F5F4),
          border: Border.all(color: agent ? const Color(0xFFFED7AA) : const Color(0xFFE7E5E4)),
          borderRadius: BorderRadius.circular(14),
        ),
        child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          Text(agent ? 'AI agent' : 'Customer', style: const TextStyle(fontSize: 11, color: Colors.black54)),
          Text(line.text),
        ]),
      ),
    );
  }
}

class _Pill extends StatelessWidget {
  final String text;
  final Color? bg;
  final Color? fg;
  const _Pill(this.text, {this.bg, this.fg});
  @override
  Widget build(BuildContext context) => Container(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 2),
        decoration: BoxDecoration(color: bg ?? const Color(0xFFF5F5F4), borderRadius: BorderRadius.circular(999)),
        child: Text(text, style: TextStyle(fontSize: 12, fontWeight: FontWeight.w600, color: fg ?? const Color(0xFF44403C))),
      );
}

class _H extends StatelessWidget {
  final String text;
  const _H(this.text);
  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.only(top: 16, bottom: 6),
        child: Text(text, style: const TextStyle(fontWeight: FontWeight.w700)),
      );
}

class _Section extends StatelessWidget {
  final String title;
  final List<Widget> children;
  const _Section({required this.title, required this.children});
  @override
  Widget build(BuildContext context) => Card(
        color: Colors.white,
        margin: const EdgeInsets.only(bottom: 14),
        child: Padding(
          padding: const EdgeInsets.all(16),
          child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
            Text(title, style: const TextStyle(fontSize: 16, fontWeight: FontWeight.w700)),
            const SizedBox(height: 12),
            ...children,
          ]),
        ),
      );
}

// ---------------------------------------------------------------- SETTINGS

class SettingsPage extends StatefulWidget {
  final VoidCallback onSaved;
  const SettingsPage({super.key, required this.onSaved});
  @override
  State<SettingsPage> createState() => _SettingsPageState();
}

class _SettingsPageState extends State<SettingsPage> {
  final key = TextEditingController(text: AppSettings.apiKey);
  final phoneId = TextEditingController(text: AppSettings.phoneNumberId);
  final agent = TextEditingController(text: AppSettings.agentName);
  final maxSeconds = TextEditingController(text: '${AppSettings.maxSeconds}');
  final model = TextEditingController(text: AppSettings.model);
  bool male = AppSettings.maleVoice;
  bool hideKey = true;
  bool checking = false;
  String? msg;
  bool ok = false;

  Future<void> _save() async {
    FocusScope.of(context).unfocus();
    AppSettings.apiKey = key.text.trim();
    AppSettings.phoneNumberId = phoneId.text.trim();
    AppSettings.agentName = agent.text.trim().isEmpty ? 'Priya' : agent.text.trim();
    AppSettings.maxSeconds = (int.tryParse(maxSeconds.text.trim()) ?? 120).clamp(30, 600);
    AppSettings.model = model.text.trim().isEmpty ? 'gpt-4o-mini' : model.text.trim();
    AppSettings.maleVoice = male;
    await AppSettings.save();
    widget.onSaved();
    if (!AppSettings.configured) {
      setState(() {
        ok = false;
        msg = 'Saved. Add both the private key and the Phone Number ID to make calls.';
      });
      return;
    }
    setState(() {
      checking = true;
      msg = null;
    });
    try {
      final p = await Vapi(AppSettings.apiKey).getPhoneNumber(AppSettings.phoneNumberId);
      setState(() {
        ok = true;
        msg = 'Connected. Calls will go out from ${p['number'] ?? 'your Vapi number'}'
            '${p['provider'] != null ? ' (${p['provider']})' : ''}.';
      });
    } catch (e) {
      setState(() {
        ok = false;
        msg = e.toString();
      });
    } finally {
      if (mounted) setState(() => checking = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        _Section(title: 'Settings', children: [
          const Text('Saved only on this phone.', style: TextStyle(color: Colors.black54)),
          const SizedBox(height: 12),
          TextField(
            controller: key,
            obscureText: hideKey,
            decoration: InputDecoration(
              labelText: 'Vapi private API key',
              suffixIcon: IconButton(
                icon: Icon(hideKey ? Icons.visibility : Icons.visibility_off),
                onPressed: () => setState(() => hideKey = !hideKey),
              ),
            ),
          ),
          const SizedBox(height: 12),
          TextField(controller: phoneId, decoration: const InputDecoration(labelText: 'Vapi Phone Number ID')),
          const SizedBox(height: 12),
          TextField(controller: agent, decoration: const InputDecoration(labelText: 'Agent name')),
          const SizedBox(height: 12),
          SegmentedButton<bool>(
            segments: const [
              ButtonSegment(value: false, label: Text('Female voice')),
              ButtonSegment(value: true, label: Text('Male voice')),
            ],
            selected: {male},
            onSelectionChanged: (s) => setState(() => male = s.first),
          ),
          const SizedBox(height: 12),
          TextField(
            controller: maxSeconds,
            keyboardType: TextInputType.number,
            decoration: const InputDecoration(labelText: 'Max call length (seconds)'),
          ),
          const SizedBox(height: 12),
          TextField(controller: model, decoration: const InputDecoration(labelText: 'AI model')),
          const SizedBox(height: 16),
          SizedBox(
            width: double.infinity,
            child: FilledButton(
              style: FilledButton.styleFrom(padding: const EdgeInsets.symmetric(vertical: 14)),
              onPressed: checking ? null : _save,
              child: Text(checking ? 'Checking with Vapi...' : 'Save & test connection'),
            ),
          ),
          if (msg != null)
            Padding(
              padding: const EdgeInsets.only(top: 10),
              child: Text(msg!, style: TextStyle(color: ok ? const Color(0xFF15803D) : const Color(0xFFB91C1C))),
            ),
        ]),
        const _Section(title: 'One-time setup (about 20 minutes)', children: [
          Text('1. Twilio: sign up at twilio.com and buy a US (+1) number with Voice. In Voice > Settings > '
              'Geo permissions, tick India. Copy the Account SID and Auth Token. A free trial can only call '
              'numbers you verify in Twilio and plays a short trial message first.'),
          SizedBox(height: 8),
          Text('2. Vapi: sign up at dashboard.vapi.ai. Phone Numbers > Import > Twilio, paste the number, '
              'SID and token. Copy the Phone Number ID.'),
          SizedBox(height: 8),
          Text('3. In Vapi, open API Keys and copy the Private key.'),
          SizedBox(height: 8),
          Text('4. Paste both above, tap Save & test, then make your first call from the Call tab.'),
          SizedBox(height: 12),
          Text('Only call people who enquired or agreed to be contacted (TRAI rules). Customers see a +1 number; '
              'switch to an Indian line (Plivo / Exotel) before going live.',
              style: TextStyle(color: Colors.black54, fontSize: 13)),
        ]),
      ],
    );
  }
}
