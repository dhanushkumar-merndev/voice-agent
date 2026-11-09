import 'package:flutter_test/flutter_test.dart';
import 'package:ai_caller/agent.dart';

void main() {
  test('normalises Indian numbers', () {
    expect(normalisePhone('98765 43210'), '+919876543210');
    expect(normalisePhone('098765-43210'), '+919876543210');
    expect(normalisePhone('919876543210'), '+919876543210');
    expect(normalisePhone('+1 415 555 0100'), '+14155550100');
    expect(normalisePhone('12'), isNull);
  });

  test('builds a Vapi call body', () {
    final r = CallRequest(
      sector: sectors.first,
      company: 'Chefpy',
      offer: sectors.first.offer,
      questions: sectors.first.questions,
      customerName: 'Rahul',
      number: '+919876543210',
      city: 'Pune',
      language: 'hinglish',
      agentName: 'Priya',
      maleVoice: false,
      model: 'gpt-4o-mini',
      maxSeconds: 120,
    );
    final body = buildCallBody(r, 'pn1');
    expect(body['phoneNumberId'], 'pn1');
    expect((body['customer'] as Map)['number'], '+919876543210');
    final a = body['assistant'] as Map;
    expect((a['voice'] as Map)['voiceId'], 'hi-IN-SwaraNeural');
    expect(a['firstMessage'], contains('Rahul'));
    final prompt = ((a['model'] as Map)['messages'] as List).first['content'] as String;
    expect(prompt, contains('How many guests'));
  });
}
