// Vapi API client, call records, local history and background call tracking.
import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:http/http.dart' as http;
import 'package:shared_preferences/shared_preferences.dart';

class AppSettings {
  static String apiKey = '';
  static String phoneNumberId = '';
  static String agentName = 'Priya';
  static bool maleVoice = false;
  static int maxSeconds = 120;
  static String model = 'gpt-4o-mini';
  static String language = 'hinglish';
  static String lastSector = 'chef';

  static bool get configured => apiKey.isNotEmpty && phoneNumberId.isNotEmpty;

  static Future<void> load() async {
    final p = await SharedPreferences.getInstance();
    apiKey = p.getString('apiKey') ?? '';
    phoneNumberId = p.getString('phoneNumberId') ?? '';
    agentName = p.getString('agentName') ?? 'Priya';
    maleVoice = p.getBool('maleVoice') ?? false;
    maxSeconds = p.getInt('maxSeconds') ?? 120;
    model = p.getString('model') ?? 'gpt-4o-mini';
    language = p.getString('language') ?? 'hinglish';
    lastSector = p.getString('lastSector') ?? 'chef';
  }

  static Future<void> save() async {
    final p = await SharedPreferences.getInstance();
    await p.setString('apiKey', apiKey);
    await p.setString('phoneNumberId', phoneNumberId);
    await p.setString('agentName', agentName);
    await p.setBool('maleVoice', maleVoice);
    await p.setInt('maxSeconds', maxSeconds);
    await p.setString('model', model);
    await p.setString('language', language);
    await p.setString('lastSector', lastSector);
  }
}

class VapiException implements Exception {
  final String message;
  VapiException(this.message);
  @override
  String toString() => message;
}

class Vapi {
  static const String base = 'https://api.vapi.ai';
  final String key;
  Vapi(this.key);

  Map<String, String> get _headers => {
        'Authorization': 'Bearer $key',
        'Content-Type': 'application/json',
      };

  Future<dynamic> _send(String method, String path, [Map<String, dynamic>? body]) async {
    final uri = Uri.parse('$base$path');
    http.Response r;
    try {
      r = method == 'POST'
          ? await http.post(uri, headers: _headers, body: jsonEncode(body)).timeout(const Duration(seconds: 30))
          : await http.get(uri, headers: _headers).timeout(const Duration(seconds: 30));
    } on TimeoutException {
      throw VapiException('Vapi did not respond. Check your internet connection.');
    } catch (e) {
      throw VapiException('Could not reach Vapi. Check your internet connection.');
    }
    dynamic data;
    try {
      data = r.body.isEmpty ? <String, dynamic>{} : jsonDecode(utf8.decode(r.bodyBytes));
    } catch (_) {
      data = {'message': r.body};
    }
    if (r.statusCode >= 300) {
      if (r.statusCode == 401) throw VapiException('Vapi rejected the key. Check the private key in Settings.');
      String msg = r.body;
      if (data is Map) {
        final m = data['message'] ?? data['error'];
        if (m is List) {
          msg = m.join('; ');
        } else if (m != null) {
          msg = m.toString();
        }
      }
      throw VapiException('Vapi error ${r.statusCode}: $msg');
    }
    return data;
  }

  Future<Map<String, dynamic>> createCall(Map<String, dynamic> body) async =>
      Map<String, dynamic>.from(await _send('POST', '/call', body) as Map);

  Future<Map<String, dynamic>> getCall(String id) async =>
      Map<String, dynamic>.from(await _send('GET', '/call/${Uri.encodeComponent(id)}') as Map);

  Future<Map<String, dynamic>> getPhoneNumber(String id) async =>
      Map<String, dynamic>.from(await _send('GET', '/phone-number/${Uri.encodeComponent(id)}') as Map);
}

class ChatLine {
  final String role; // agent | customer
  final String text;
  ChatLine(this.role, this.text);
  Map<String, dynamic> toJson() => {'role': role, 'text': text};
  factory ChatLine.fromJson(Map<String, dynamic> j) => ChatLine('${j['role']}', '${j['text']}');
}

class CallRecord {
  String id;
  String number;
  String name;
  String company;
  String createdAt;
  String status;
  String? endedReason;
  int? durationSeconds;
  double? cost;
  String? summary;
  Map<String, dynamic>? lead;
  String? transcript;
  List<ChatLine> messages;
  String? recordingUrl;

  CallRecord({
    required this.id,
    required this.number,
    required this.name,
    required this.company,
    required this.createdAt,
    required this.status,
    this.endedReason,
    this.durationSeconds,
    this.cost,
    this.summary,
    this.lead,
    this.transcript,
    List<ChatLine>? messages,
    this.recordingUrl,
  }) : messages = messages ?? [];

  bool get ended => status == 'ended';
  bool get hasReport => summary != null || lead != null;

  void apply(Map<String, dynamic> c) {
    status = (c['status'] ?? status).toString();
    endedReason = c['endedReason']?.toString() ?? endedReason;
    final cst = c['cost'];
    if (cst is num) cost = cst.toDouble();
    final s = DateTime.tryParse('${c['startedAt'] ?? ''}');
    final e = DateTime.tryParse('${c['endedAt'] ?? ''}');
    if (s != null && e != null) durationSeconds = e.difference(s).inSeconds.abs();
    final a = c['analysis'] is Map ? Map<String, dynamic>.from(c['analysis'] as Map) : <String, dynamic>{};
    final art = c['artifact'] is Map ? Map<String, dynamic>.from(c['artifact'] as Map) : <String, dynamic>{};
    summary = (a['summary'] ?? c['summary'])?.toString() ?? summary;
    if (a['structuredData'] is Map) lead = Map<String, dynamic>.from(a['structuredData'] as Map);
    transcript = (art['transcript'] ?? c['transcript'])?.toString() ?? transcript;
    recordingUrl = (art['recordingUrl'] ?? c['recordingUrl'])?.toString() ?? recordingUrl;
    final raw = art['messages'] ?? c['messages'];
    if (raw is List) {
      final lines = <ChatLine>[];
      for (final m in raw) {
        if (m is! Map) continue;
        final role = '${m['role']}';
        final text = (m['message'] ?? m['content'])?.toString() ?? '';
        if (text.isEmpty) continue;
        if (role == 'bot' || role == 'assistant') lines.add(ChatLine('agent', text));
        if (role == 'user') lines.add(ChatLine('customer', text));
      }
      if (lines.isNotEmpty) messages = lines;
    }
  }

  Map<String, dynamic> toJson() => {
        'id': id,
        'number': number,
        'name': name,
        'company': company,
        'createdAt': createdAt,
        'status': status,
        'endedReason': endedReason,
        'durationSeconds': durationSeconds,
        'cost': cost,
        'summary': summary,
        'lead': lead,
        'transcript': transcript,
        'messages': messages.map((m) => m.toJson()).toList(),
        'recordingUrl': recordingUrl,
      };

  factory CallRecord.fromJson(Map<String, dynamic> j) => CallRecord(
        id: '${j['id']}',
        number: '${j['number'] ?? ''}',
        name: '${j['name'] ?? ''}',
        company: '${j['company'] ?? ''}',
        createdAt: '${j['createdAt'] ?? ''}',
        status: '${j['status'] ?? 'queued'}',
        endedReason: j['endedReason']?.toString(),
        durationSeconds: (j['durationSeconds'] as num?)?.toInt(),
        cost: (j['cost'] as num?)?.toDouble(),
        summary: j['summary']?.toString(),
        lead: j['lead'] is Map ? Map<String, dynamic>.from(j['lead'] as Map) : null,
        transcript: j['transcript']?.toString(),
        messages: j['messages'] is List
            ? (j['messages'] as List).whereType<Map>().map((m) => ChatLine.fromJson(Map<String, dynamic>.from(m))).toList()
            : null,
        recordingUrl: j['recordingUrl']?.toString(),
      );

  String reportText() {
    final l = lead ?? {};
    final b = StringBuffer();
    b.writeln('AI call report: ${name.isNotEmpty ? '$name ' : ''}$number');
    if (company.isNotEmpty) b.writeln('Company: $company');
    final outcome = l['call_outcome'];
    if (outcome != null) {
      b.writeln('Outcome: ${outcome.toString().replaceAll('_', ' ')}'
          '${l['lead_temperature'] != null ? ' (${l['lead_temperature']} lead)' : ''}');
    }
    if (summary != null) b.writeln('\n$summary');
    final details = l.entries.where((e) => e.key != 'call_outcome' && e.key != 'lead_temperature' && e.value != null && '${e.value}'.isNotEmpty);
    if (details.isNotEmpty) b.writeln();
    for (final e in details) {
      final v = e.value == true ? 'Yes' : e.value == false ? 'No' : '${e.value}';
      b.writeln('${e.key.replaceAll('_', ' ')}: $v');
    }
    if (recordingUrl != null) b.writeln('\nRecording: $recordingUrl');
    return b.toString().trim();
  }
}

/// Bumped every time a call record changes, so screens can rebuild.
final ValueNotifier<int> storeVersion = ValueNotifier<int>(0);

class Store {
  static const _key = 'calls_v1';
  static Future<void> _lock = Future.value();

  static Future<List<CallRecord>> all() async {
    final p = await SharedPreferences.getInstance();
    final raw = p.getString(_key);
    if (raw == null) return [];
    try {
      return (jsonDecode(raw) as List)
          .whereType<Map>()
          .map((e) => CallRecord.fromJson(Map<String, dynamic>.from(e)))
          .toList();
    } catch (_) {
      return [];
    }
  }

  static Future<void> upsert(CallRecord r) {
    final next = _lock.then((_) async {
      final list = await all();
      list.removeWhere((x) => x.id == r.id);
      list.insert(0, r);
      if (list.length > 300) list.removeRange(300, list.length);
      final p = await SharedPreferences.getInstance();
      await p.setString(_key, jsonEncode(list.map((e) => e.toJson()).toList()));
      storeVersion.value++;
    });
    _lock = next.catchError((Object _) {});
    return next;
  }

  static Future<void> clear() async {
    final p = await SharedPreferences.getInstance();
    await p.remove(_key);
    storeVersion.value++;
  }
}

final Set<String> _tracking = {};

/// Polls Vapi until the call has ended and its report (summary / details) has arrived.
Future<void> trackCall(CallRecord rec) async {
  if (_tracking.contains(rec.id) || AppSettings.apiKey.isEmpty) return;
  _tracking.add(rec.id);
  final api = Vapi(AppSettings.apiKey);
  final started = DateTime.now();
  DateTime? endedSeen;
  try {
    while (DateTime.now().difference(started).inMinutes < 20) {
      try {
        rec.apply(await api.getCall(rec.id));
        await Store.upsert(rec);
        if (rec.ended) {
          final since = endedSeen ??= DateTime.now();
          if (rec.hasReport || DateTime.now().difference(since).inSeconds > 60) return;
        }
      } catch (_) {
        // network blips on mobile are normal; keep trying
      }
      await Future<void>.delayed(const Duration(seconds: 3));
    }
  } finally {
    _tracking.remove(rec.id);
  }
}
